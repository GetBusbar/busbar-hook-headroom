// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors

//! `busbar-hook-headroom` — busbar's first-party **`headroom`** `kind: hook` plugin: a signed,
//! trusted, dlopen'd `prompt: rw` REWRITE GATE that compresses LLM chat history with the REAL
//! `headroom-core` engine (BM25 `TextCrusher`), not a placeholder. A `cdylib` implementing the
//! plugin SDK's [`Hook`] trait; the engine calls it in-process through the dlopen ABI.
//!
//! ## Ops
//!
//! - **`transform`** — the compression pass ([`compress::run_transform`]). Reads
//!   `request.messages` from the projection (only present when the operator granted `prompt: rw`),
//!   keeps the LAST message (the ask) and any in-turn `system`/`developer` turn verbatim, and
//!   BM25-compresses every other message against the ask. It never rewrites a prompt that does
//!   not end on a `user` turn, that has a turn projected as role `tool`/`function`, or whose
//!   hoisted `request.system` rides in the turns array the rewrite replaces (every dialect but
//!   anthropic, bedrock and gemini). Returns `{"rewrite": {"messages": [...]}}` (BODY form) IFF
//!   whole-prompt byte savings clear `min_savings_pct`; otherwise `{}` abstain. Never fails the
//!   request — `TextCrusher` (third-party code) runs under `catch_unwind` per message.
//! - **`decide` / `notify`** — ABSTAIN / no-op. This hook is a rewrite gate, not a router or a
//!   tap.
//! - **`configure`** — applies a pushed settings map as DESIRED STATE ([`compress::apply_settings`]):
//!   present keys override, absent keys reset to the built-in default, and an unknown key or an
//!   out-of-range/wrong-type value NACKs (the live `Knobs` are left untouched).
//! - **`describe`** — the `{schema, dashboard}` self-description ([`compress::describe_reply`]).
//! - **`status`** — observed settings + a Prometheus-shaped metrics array using Headroom's REAL
//!   metric names ([`compress::build_status`]), plus the `headroom-core` git ref this binary was
//!   built against (stamped by `build.rs` from `Cargo.lock`). busbar 1.6.0 reads only
//!   `settings_version`/`settings`/`metrics` from the reply, so that ref is not surfaced there.
//!
//! ## The manifest `needs`
//!
//! Headroom is a `prompt: rw` gate: its signed packaging manifest declares `needs.prompt = rw`. Even
//! with that declared, the engine only projects the prompt into the `transform` payload when the
//! OPERATOR also grants `prompt: rw`; absent that grant the payload carries no `request.messages`
//! and this gate abstains — it can never coerce content.

mod compress;

use busbar_contract::abi::sdk::exchange::Op;
use busbar_contract::abi::sdk::hook::{
    ConfigureCheckpoint, Decoded, Hook, HookOpen, RewriteVerdict, lower_transform_reply,
};
use busbar_contract::plugin::markers::HookKind;
use compress::{Knobs, Metrics, build_status, describe_reply, run_transform};
use serde_json::{Map, Value};
use std::sync::{Mutex, RwLock};
use std::task::Poll;

/// The `headroom-core` git ref this binary was built against (tag when pinned to a release, else a
/// short commit) — stamped by `build.rs` from `Cargo.lock`.
const HEADROOM_CORE_REF: &str = env!("HEADROOM_CORE_REF");

/// The live gate: compression knobs behind an `RwLock` (the SDK's [`Hook`] methods take
/// `&self` — the engine may drive `transform`/`configure` concurrently — so committing a `configure`
/// push is interior mutability) plus the
/// per-instance, per-pool metrics accumulator (busbar opens a fresh instance on a config reload or
/// settings PATCH, so the counters restart then).
struct Headroom {
    knobs: RwLock<Knobs>,
    metrics: Mutex<Metrics>,
    revision: Mutex<u64>,
}

impl Headroom {
    fn new(knobs: Knobs) -> Self {
        Headroom {
            knobs: RwLock::new(knobs),
            metrics: Mutex::new(Metrics::default()),
            revision: Mutex::new(0),
        }
    }
}

impl Headroom {
    fn apply_configuration(
        &self,
        settings: &Map<String, Value>,
        version: u64,
    ) -> Result<ConfigureCheckpoint, String> {
        let mut knobs = compress::apply_settings(settings)
            .map_err(|why| format!("headroom: configure v{version} rejected: {why}"))?;
        knobs.settings_version = version;
        let mut revision = self.revision.lock().unwrap_or_else(|e| e.into_inner());
        let next = revision
            .checked_add(1)
            .ok_or("headroom: configuration revision exhausted")?;
        // Desired-state settings reconstruct all knobs, including defaults for absent keys.
        // Validate and own the complete checkpoint before changing the live configuration.
        let checkpoint = ConfigureCheckpoint::new(
            next,
            version,
            serde_json::to_vec(&knobs.pushed).map_err(|_| "headroom: cannot encode settings")?,
        )?;
        *self.knobs.write().unwrap_or_else(|e| e.into_inner()) = knobs;
        *revision = next;
        Ok(checkpoint)
    }

    #[cfg(test)]
    fn transform_payload(&self, payload: &Value) -> Value {
        run_transform(payload, &self.knobs, &self.metrics)
    }
}

impl Hook for Headroom {
    fn transform(&self, view: &Decoded<'_>, _op: &Op<'_, HookKind>) -> Poll<RewriteVerdict> {
        Poll::Ready(lower_transform_reply(Ok(run_transform(
            &view.projection_json("transform"),
            &self.knobs,
            &self.metrics,
        ))))
    }

    fn configure(&self, settings: &Map<String, Value>, version: u64) -> Result<(), String> {
        self.apply_configuration(settings, version).map(|_| ())
    }

    fn configure_checkpoint(
        &self,
        settings: &Map<String, Value>,
        version: u64,
    ) -> Result<Option<ConfigureCheckpoint>, String> {
        self.apply_configuration(settings, version).map(Some)
    }

    /// `describe` — the `{schema, dashboard}` self-description envelope.
    fn describe(&self) -> Value {
        describe_reply()
    }

    /// `status` — observed settings + the metrics array, plus the `headroom-core` build ref.
    fn status(&self) -> Value {
        let mut reply = build_status(&self.knobs, &self.metrics);
        if let Some(status) = reply.get_mut("status").and_then(|s| s.as_object_mut()) {
            status.insert(
                "headroom_core_ref".to_string(),
                Value::String(HEADROOM_CORE_REF.to_string()),
            );
        }
        reply
    }
}

/// Construct the gate from the engine-passed JSON config (the operator's `settings:` map, the same
/// shape a `configure` push carries). An empty/missing config uses the built-in defaults. A
/// MALFORMED config (bad JSON, unknown key, out-of-range value) is a fail-closed LOAD error — better
/// a loud load failure than a silently mis-parsed knob.
fn open(cfg: &str) -> Result<Box<dyn Hook>, String> {
    let knobs = if cfg.trim().is_empty() {
        Knobs::default()
    } else {
        let settings: Map<String, Value> = serde_json::from_str(cfg).map_err(|e| {
            format!(
                "headroom-hook: invalid plugin config at line {}, column {}",
                e.line(),
                e.column()
            )
        })?;
        compress::apply_settings(&settings)
            .map_err(|e| format!("headroom-hook: invalid settings: {e}"))?
    };
    Ok(Box::new(Headroom::new(knobs)))
}

struct Open;
impl HookOpen for Open {
    fn open(settings: &str) -> Result<Box<dyn Hook>, String> {
        open(settings)
    }
}

const REWRITES: &[busbar_contract::abi::mechanism::door::Rewrite] =
    &[busbar_contract::abi::sdk::door::alias("headroom")];
const TAIL: &busbar_contract::abi::hook::Tail = &busbar_contract::abi::sdk::hook::tail(
    busbar_contract::abi::hook::CLASS_GATE,
    busbar_contract::abi::hook::PROMPT_RW,
    busbar_contract::abi::hook::USER_NO,
);
const STATEMENT: busbar_contract::abi::mechanism::door::Statement =
    busbar_contract::abi::mechanism::door::Statement {
        rewrites: REWRITES.as_ptr(),
        rewrites_len: REWRITES.len(),
        ..busbar_contract::abi::sdk::hook::statement_with_tail(
            busbar_contract::abi::sdk::door::statement(
                "busbar-hook-headroom",
                env!("CARGO_PKG_VERSION"),
                64,
            ),
            TAIL,
        )
    };
busbar_contract::hook_door! { open: Open, statement: STATEMENT, }
busbar_contract::export_door!(door);

#[cfg(test)]
#[path = "tests/lib.rs"]
mod tests;
