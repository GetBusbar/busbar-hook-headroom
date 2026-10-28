// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors

//! `headroom-hook` — busbar's first-party **`headroom`** `kind: hook` plugin: a signed, trusted,
//! dlopen'd `prompt: rw` REWRITE GATE that compresses LLM chat history with the REAL
//! `headroom-core` engine (BM25 `TextCrusher`), not a placeholder.
//!
//! Formerly a standalone Unix-socket hook service (`main.rs`, now deleted) speaking busbar's
//! 5-message wire (`configure`/`describe`/`decide`/`transform`/`notify`) as newline-delimited JSON
//! over a kept-alive connection. Now a `cdylib` whose door is the hook kind's (`hook_door!`, over
//! `plugin_door!`; see [`door`]) — the wire-line dispatch is gone (the engine calls in-process
//! through the plugin ABI), but the compression logic in [`compress`] is a straight port of the old
//! socket hook's `handle_line`/`build_status`/`apply_settings`.
//!
//! ## Ops
//!
//! - **`transform`** — the compression pass ([`compress::run_transform`]). Reads `system`-adjacent
//!   `request.messages` from the projection (only present when the operator granted `prompt: rw`),
//!   keeps the LAST message verbatim as the ask, and BM25-compresses every earlier message against
//!   it. Returns `{"rewrite": {"messages": [...]}}` (BODY form) IFF whole-prompt char savings clear
//!   `min_savings_pct`; otherwise `{}` abstain. Never fails the request — `TextCrusher` (third-party
//!   code) runs under `catch_unwind` per message.
//! - **`decide` / `notify`** — ABSTAIN / no-op. This hook is a rewrite gate, not a router or a tap
//!   (the old wire never used it for either).
//! - **`configure`** — applies a pushed settings map as DESIRED STATE ([`compress::apply_settings`]):
//!   present keys override, absent keys reset to the built-in default, and an unknown key or an
//!   out-of-range/wrong-type value NACKs (the live `Knobs` are left untouched).
//! - **`describe`** — the `{schema, dashboard}` self-description ([`compress::describe_reply`]).
//! - **`status`** — observed settings + a Prometheus-shaped metrics array using Headroom's REAL
//!   metric names ([`compress::build_status`]), plus the `headroom-core` git ref this binary was
//!   built against (stamped by `build.rs` from `Cargo.lock`).
//!
//! ## The manifest `needs`
//!
//! Headroom is a `prompt: rw` gate: its signed packaging manifest declares `needs.prompt = rw`. Even
//! with that declared, the engine only projects the prompt into the `transform` payload when the
//! OPERATOR also grants `prompt: rw`; absent that grant the payload carries no `request.messages`
//! and this gate abstains — it can never coerce content.

mod compress;
pub mod door;

use compress::{Knobs, Metrics, build_status, describe_reply, run_transform};
use serde_json::{Map, Value};
use std::sync::{Mutex, RwLock};

/// The `headroom-core` git ref this binary was built against (tag when pinned to a release, else a
/// short commit) — stamped by `build.rs` from `Cargo.lock`.
const HEADROOM_CORE_REF: &str = env!("HEADROOM_CORE_REF");

/// The package name a signed tarball of this plugin states (`manifest.name`), and its Statement's.
pub const NAME: &str = "busbar-hook-headroom";

/// The alias a hook reference names this plugin by (`module: headroom`).
pub const ALIAS: &str = "headroom";

/// The live gate: compression knobs behind an `RwLock` (the SDK's `Hook` methods take
/// `&self` — the engine may drive `transform`/`configure` concurrently — so committing a `configure`
/// push is interior mutability, mirroring the old socket hook's `Arc<RwLock<Knobs>>`) plus the
/// process-wide per-pool metrics accumulator.
pub struct Headroom {
    knobs: RwLock<Knobs>,
    metrics: Mutex<Metrics>,
}

impl Headroom {
    fn new(knobs: Knobs) -> Self {
        Headroom {
            knobs: RwLock::new(knobs),
            metrics: Mutex::new(Metrics::default()),
        }
    }
}

/// The gate's ops over the 1.5.5 JSON documents; [`door`] puts them on the hook kind's door.
/// `decide` and `notify` are the SDK's defaults there (ABSTAIN / nothing): Headroom ranks nothing
/// and is not a tap.
impl Headroom {
    /// `transform` — compress the granted history, keep the ask. See [`compress::run_transform`].
    pub fn transform(&self, payload: &Value) -> Value {
        run_transform(payload, &self.knobs, &self.metrics)
    }

    /// `configure` — apply a pushed settings map as DESIRED STATE and commit it live iff it applies
    /// cleanly; an unknown key or an out-of-shape/out-of-range value NACKs (returns `false`) and the
    /// live knobs are left untouched, matching the old wire's commit-on-ack fail-closed contract.
    pub fn configure(&self, settings: &Map<String, Value>, settings_version: u64) -> bool {
        match compress::apply_settings(settings) {
            Ok(mut new) => {
                new.settings_version = settings_version;
                // Poison-tolerant: `Knobs` is `Copy` and writes are single assignments, so a
                // poisoned lock holds a fully-written value — recover it, never panic on the
                // request path.
                *self.knobs.write().unwrap_or_else(|e| e.into_inner()) = new;
                true
            }
            Err(e) => {
                // The call capture carries it to the plugin's log (`abi::sdk::capture`).
                log::warn!("headroom: configure v{settings_version} rejected: {e}");
                false
            }
        }
    }

    /// `describe` — the `{schema, dashboard}` self-description envelope.
    pub fn describe(&self) -> Value {
        describe_reply()
    }

    /// `status` — observed settings + the metrics array, plus the `headroom-core` build ref.
    pub fn status(&self) -> Value {
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
pub fn open(cfg: &str) -> Result<Headroom, String> {
    let knobs = if cfg.trim().is_empty() {
        Knobs::default()
    } else {
        let settings: Map<String, Value> = serde_json::from_str(cfg)
            .map_err(|e| format!("headroom-hook: invalid plugin config: {e}"))?;
        compress::apply_settings(&settings)
            .map_err(|e| format!("headroom-hook: invalid settings: {e}"))?
    };
    Ok(Headroom::new(knobs))
}

/// THE DROPPED-IN DOOR: this image's ONE symbol, `busbar_plugin_door`, forwarding to [`door::door`]
/// (`export_door!`). The macro's `#[no_mangle]` is the only unsafe attribute in the crate.
mod exported {
    busbar_contract::export_door!(crate::door::door);
}

/// THE LINKED ENTRY: what a busbar build that compiles this gate in registers onto the hook axis —
/// the same name and alias a dropped-in signed tarball of this crate's `cdylib` states, and the same
/// door ([`door::door`]) the tarball's `busbar_plugin_door` forwards to. One source, both doors.
pub mod linked {
    /// `(name, alias, door)`.
    pub const HOOK: (&str, &str, busbar_contract::abi::mechanism::door::DoorFn) =
        (super::NAME, super::ALIAS, super::door::door);
}

#[cfg(test)]
#[path = "tests/lib.rs"]
mod tests;
