// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors

//! End-to-end coverage of the `headroom-hook` compression gate loaded over the REAL
//! `busbar-plugin-loader` hook seam — the exact seam busbar's engine sees: the built cdylib
//! admitted by the one loader (`load_dropped`, against its door's own Statement rendering), bound to
//! a real dispatcher, and opened and called as the kernel calls a hook (`hook_door::HookInstance`,
//! the contract's `HookCalls`): every `decide`/`transform` over the host's view of the request
//! (`DecideView`, the granted prompt included), whose `transform` compresses that prompt with the
//! REAL `headroom-core` BM25 `TextCrusher` engine and answers through the hook kind's fixed `out`
//! and the host's rewrite buffer.
//!
//! The corpus fixtures (`log_dump`) are shared with `src/tests/compress.rs`: a toy 2-word string
//! never triggers BM25/TextCrusher's real compression path, since `TextCrusher` passes short texts
//! (<6 segments) through unchanged.

use busbar_contract::abi::hook::{VERB_ABSTAIN, VERB_REWRITE};
use busbar_contract::abi::host::hook::{DecideFrame, DecideView};
use busbar_contract::abi::mechanism::call::Outcome;
use busbar_contract::hook_calls::{Answered, HookCalls};
use busbar_contract::hooks::{Candidate, RoutingContext, RoutingRequest};
use busbar_plugin_loader::dispatch::kinds::hook::Hook;
use busbar_plugin_loader::dispatch::{
    Bind, ConnTable, DispatchConfig, Dispatcher, NoSink, load_dropped, rendering_of,
};
use busbar_plugin_loader::hook_door::HookInstance;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Locate the built `headroom-hook` cdylib in the target dir (mirrors the loader's hook_plugin_path).
/// `[package] name = "headroom-hook"` -> cargo's cdylib filename is `libbusbar_hook_headroom.{so,dylib}`.
///
/// Checks BOTH `<profile_dir>/<name>` (the "uplifted" copy Cargo produces only when `[lib]` is a
/// ROOT build target of the invocation, e.g. `cargo build --all-targets`) AND
/// `<profile_dir>/deps/<name>` (the raw compiler output, refreshed on EVERY build that recompiles
/// the lib, uplifted or not). A bare `cargo test` (what this repo's own CI runs) does NOT uplift
/// the cdylib to the top-level profile dir, only to `target/deps` — checking only `profile_dir`
/// silently finds nothing even though the cdylib really was built, so every test below would
/// no-op via this function's `None` return, appearing to pass without exercising a single line of
/// the real cdylib.
fn plugin_path() -> Option<std::path::PathBuf> {
    let candidate = (|| {
        let exe = std::env::current_exe().ok()?;
        let profile_dir = exe.parent()?.parent()?;
        let name = busbar_plugin_loader::plugin_library_filename("busbar_hook_headroom");
        let uplifted = profile_dir.join(&name);
        let raw = profile_dir.join("deps").join(&name);
        [uplifted, raw]
            .into_iter()
            .filter_map(|p| {
                std::fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .ok()
                    .map(|mtime| (p, mtime))
            })
            .max_by_key(|(_, mtime)| *mtime)
            .map(|(p, _)| p)
    })();
    if candidate.is_none() && std::env::var_os("CI").is_some() {
        panic!(
            "the headroom-hook cdylib is not built under CI: `cargo test` must build it \
             (checked both the uplifted target dir and target/deps). Refusing to silently skip \
             real coverage."
        );
    }
    candidate
}

/// A synthetic log-dump line set: many segments, mostly noise, one load-bearing ERROR — realistic
/// enough for BM25 relevance scoring to actually distinguish signal from noise (unlike a toy
/// 2-word string, which TextCrusher passes through unchanged).
fn log_dump(lines: usize) -> String {
    let mut s = String::new();
    for i in 0..lines {
        if i == lines / 2 {
            s.push_str("ERROR: deployment canary failed with status 503 on us-east-1.\n");
        } else {
            s.push_str(&format!(
                "Routine step {i} completed in the staging environment without incident.\n"
            ));
        }
    }
    s
}

/// What a `transform` answered, read off the hook kind's `out` and the host's rewrite buffer.
#[derive(Debug)]
enum Transformed {
    /// REWRITE: the 1.5.5 rewrite JSON's `messages` (`{"messages": [...]}`).
    Rewrite(Vec<Value>),
    /// ABSTAIN: the original body proceeds.
    Abstain,
    /// Anything else (a reject, a failure, a fault), described.
    #[allow(dead_code)]
    Other(String),
}

/// The gate, opened over the real loader seam: the cdylib admitted by `load_dropped` on its own
/// dispatcher, opened and called as the kernel calls a hook.
struct Gate(HookInstance);

impl Gate {
    /// `transform` `req` (with one candidate, as a pool has).
    async fn transform(&self, req: &RoutingRequest<'_>) -> Transformed {
        let frame = DecideFrame::first(DecideView::build(req, &[cand(0)], &ctx()));
        match self.0.transform(frame, BUDGET).await {
            Answered::Answer {
                outcome: Outcome::Ready,
                out,
                frame,
                ..
            } if out.verbs & VERB_REWRITE != 0 => {
                let rw: Value = serde_json::from_slice(&frame.rewrite(out.rewrite_written))
                    .expect("the rewrite crosses as JSON");
                Transformed::Rewrite(rw["messages"].as_array().cloned().unwrap_or_default())
            }
            Answered::Answer {
                outcome: Outcome::Ready,
                out,
                ..
            } if out.verbs & VERB_ABSTAIN != 0 => Transformed::Abstain,
            other => Transformed::Other(format!("{other:?}")),
        }
    }

    /// `configure` `settings` at `version`.
    async fn configure(
        &self,
        settings: &serde_json::Map<String, Value>,
        version: u64,
    ) -> Result<(), String> {
        let text = serde_json::to_string(settings).expect("settings serialize");
        self.0.configure("headroom", &text, version, BUDGET).await
    }

    /// A leased JSON document (`status` / `describe`).
    fn document(bytes: Option<Vec<u8>>) -> Value {
        serde_json::from_slice(&bytes.expect("the gate answers the document"))
            .expect("the document is JSON")
    }
}

/// Load the gate over the real loader seam with the given `settings` JSON.
fn load(settings: &str) -> Gate {
    let path = plugin_path().expect("headroom-hook cdylib built under --workspace");
    let dispatcher = Arc::new(Dispatcher::new(DispatchConfig {
        workers: 2,
        ..Default::default()
    }));
    let bind = Bind {
        instance: Arc::from("headroom-e2e"),
        max_inflight_cap: 64,
        sink: Arc::new(NoSink),
        dispatcher: dispatcher.adopter(),
        // The gate declares no need: it is served with no connection table.
        conns: ConnTable::NoNeeds,
    };
    let stated = rendering_of(busbar_hook_headroom::door::door).expect("the door renders");
    let plugin = load_dropped::<Hook>(&path, &stated, bind)
        .expect("load the headroom-hook plugin over the ABI");
    let instance = HookInstance::open(plugin, dispatcher, settings.as_bytes(), || {
        Err("no rebind in this test".to_string())
    })
    .expect("the gate opens");
    Gate(instance)
}

/// A request with a MULTI-message, realistic history (grant given): the projector carries system +
/// every message body. `run_transform` needs >= 2 messages (history + ask) to have anything to
/// compress, and TextCrusher needs a real multi-segment body to meaningfully shrink — a log dump,
/// not a toy string.
fn req_with_history(history: String, ask: &str) -> RoutingRequest<'static> {
    let total_chars = history.len() + ask.len();
    RoutingRequest {
        // A DISTINCT non-zero correlation id per fixture (core stamps a real per-request `u64` at
        // ingress; `0` is what an unstamped/defaulted projection looks like). Nothing asserts on it
        // today, but the fixture should look like a real request, so the two fixtures stay
        // tellable apart on the view the gate is handed.
        request_id: 1001,
        pool: "p",
        ingress_protocol: "anthropic",
        requested_model: None,
        message_count: 2,
        tool_count: 0,
        has_tools: false,
        total_chars,
        system_chars: 0,
        max_tokens: None,
        stream: false,
        prompt: Some(busbar_contract::hooks::PromptProjection {
            system: None,
            messages: vec![
                ("user".into(), history.into()),
                ("user".into(), ask.to_string().into()),
            ],
        }),
        identity: None,
        // The DEFAULT (empty) declared-signal bag: this gate declares no request-phase signals, so
        // the engine hands it exactly this — an empty bag — on every real request, and the gate's
        // behaviour on that default path is what these tests assert.
        signals: Default::default(),
        session: None,
    }
}

/// A request with NO prompt (grant absent): the projector carries no prompt keys → the gate abstains.
fn req_without_prompt() -> RoutingRequest<'static> {
    RoutingRequest {
        request_id: 1002,
        pool: "p",
        ingress_protocol: "anthropic",
        requested_model: None,
        message_count: 1,
        tool_count: 0,
        has_tools: false,
        total_chars: 0,
        system_chars: 0,
        max_tokens: None,
        stream: false,
        prompt: None,
        identity: None,
        signals: Default::default(),
        session: None,
    }
}

fn cand(idx: usize) -> Candidate<'static> {
    Candidate {
        idx,
        model: "m",
        provider: "prov",
        weight: 1,
        context_max: None,
        tier: None,
        cost_per_mtok: None,
        tags: &[],
        latency_ms: None,
        available_concurrency: 1,
        budget_remaining: None,
        rate_headroom: None,
        // Candidate-phase declared signals: empty, same reason as the request bag above.
        signals: Default::default(),
    }
}

fn ctx() -> RoutingContext<'static> {
    RoutingContext {
        pool: "p",
        budget_remaining: None,
        budget: &[],
    }
}

const BUDGET: Duration = Duration::from_secs(5);

/// END-TO-END: load the gate, `transform` a realistic compressible history (grant given) → a
/// Rewrite carrying the shrunk message bodies in BODY form (`{"role","content"}` — the deliberate
/// asymmetry from the old socket hook: input is read from `text`, output is emitted as `content`).
#[tokio::test]
async fn transform_compresses_granted_history() {
    if plugin_path().is_none() {
        eprintln!("skip: headroom-hook cdylib not built (run under --workspace)");
        return;
    }
    let policy = load("{}");
    let history = format!("{}{}", log_dump(40), log_dump(40));
    let req = req_with_history(history.clone(), "why did the deployment fail");
    match policy.transform(&req).await {
        Transformed::Rewrite(messages) => {
            assert_eq!(messages.len(), 2);
            assert_eq!(messages[1]["content"], "why did the deployment fail");
            assert!(messages[0]["content"].as_str().unwrap().contains("ERROR"));
            assert!(
                messages[0].get("text").is_none(),
                "reply must be BODY form (content), not projection form (text)"
            );
            assert!(
                messages[0]["content"].as_str().unwrap().len() < history.len(),
                "the history must actually shrink"
            );
        }
        other => panic!("expected Rewrite, got {other:?}"),
    }
}

/// `transform` of an already-tight prompt → Abstain (nothing to compress; the original body proceeds).
#[tokio::test]
async fn transform_abstains_when_nothing_to_compress() {
    if plugin_path().is_none() {
        return;
    }
    let policy = load("{}");
    let req = req_with_history("hi".to_string(), "how are you");
    assert!(matches!(policy.transform(&req).await, Transformed::Abstain));
}

/// `transform` with NO prompt projected (operator grant absent) → Abstain. The gate can never coerce
/// content: without the projection it simply has nothing to rewrite.
#[tokio::test]
async fn transform_abstains_without_grant() {
    if plugin_path().is_none() {
        return;
    }
    let policy = load("{}");
    assert!(matches!(
        policy.transform(&req_without_prompt()).await,
        Transformed::Abstain
    ));
}

/// Abstain when `min_savings_pct` is pushed to 100 via `configure` — even a real, shrinking history
/// abstains once the savings bar is unreachable.
#[tokio::test]
async fn transform_abstains_when_min_savings_unreachable() {
    if plugin_path().is_none() {
        return;
    }
    let policy = load("{}");
    let mut settings = serde_json::Map::new();
    settings.insert("min_savings_pct".into(), serde_json::json!(100.0));
    policy
        .configure(&settings, 1)
        .await
        .expect("a well-formed configure push acks");

    let history = format!("{}{}", log_dump(40), log_dump(40));
    let req = req_with_history(history, "why did the deployment fail");
    assert!(matches!(policy.transform(&req).await, Transformed::Abstain));
}

/// `decide` abstains over the seam (the gate ranks nothing).
#[tokio::test]
async fn decide_abstains() {
    if plugin_path().is_none() {
        return;
    }
    let policy = load("{}");
    let req = req_with_history("hi".to_string(), "x");
    let frame = DecideFrame::first(DecideView::build(&req, &[cand(0)], &ctx()));
    match policy.0.decide(frame, BUDGET).await {
        Answered::Answer {
            outcome: Outcome::Ready,
            out,
            ..
        } => assert_eq!(out.verbs, VERB_ABSTAIN, "decide must abstain"),
        other => panic!("expected an abstaining decide, got {other:?}"),
    }
}

/// `describe` returns the settings schema + dashboard envelope; `status` reports observed settings +
/// Headroom-named metrics after a real compressing transform (tokens_saved > 0), with no prompt
/// content surfaced.
#[tokio::test]
async fn describe_and_status_over_the_seam() {
    if plugin_path().is_none() {
        return;
    }
    let policy = load(r#"{"target_ratio":0.4}"#);
    let describe = Gate::document(policy.0.describe(BUDGET).await);
    let schema = &describe["schema"];
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["target_ratio"].is_object());

    // Run a real compressing transform so status has something honest to report.
    let history = format!("{}{}", log_dump(40), log_dump(40));
    let req = req_with_history(history, "why did the deployment fail");
    match policy.transform(&req).await {
        Transformed::Rewrite(_) => {}
        other => panic!("expected Rewrite to drive status, got {other:?}"),
    }

    let status = Gate::document(policy.0.status(BUDGET).await);
    let status = &status["status"];
    let settings = status["settings"].as_object().expect("status settings");
    assert_eq!(settings.get("target_ratio").unwrap(), 0.4);
    let metrics = status["metrics"].as_array().expect("metrics");
    let saved = metrics
        .iter()
        .find(|m| m["name"] == "headroom_tokens_saved_total")
        .expect("headroom_tokens_saved_total metric");
    assert!(
        saved["value"].as_u64().unwrap() > 0,
        "compression saved tokens"
    );
}

/// `configure` over the seam acks a well-formed settings push and NACKs a malformed one.
#[tokio::test]
async fn configure_acks_and_nacks_over_the_seam() {
    if plugin_path().is_none() {
        return;
    }
    let policy = load("{}");
    let mut good = serde_json::Map::new();
    good.insert("target_ratio".into(), serde_json::json!(0.3));
    policy
        .configure(&good, 2)
        .await
        .expect("a well-formed settings push acks");

    let mut bad = serde_json::Map::new();
    bad.insert("target_ratio".into(), serde_json::json!(2.0));
    assert!(
        policy.configure(&bad, 3).await.is_err(),
        "an out-of-range target_ratio must NACK (malformed push does not commit)"
    );
}
