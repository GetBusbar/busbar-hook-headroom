// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors

//! Actual Headroom compression driven through the current typed hook ABI.
#[path = "support/door_host.rs"]
#[allow(dead_code)]
mod door_host;

use busbar_contract::hooks::{
    Candidate, RoutingContext, RoutingDecision, RoutingRequest, TransformOutcome,
};
use std::time::Duration;

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

fn load(settings: &str) -> door_host::Policy {
    door_host::Policy::dropped(settings)
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
        // today — these projectors don't carry it onto the wire — but the fixture should look like a
        // real request, so if the projection ever grows the id the two fixtures stay tellable apart.
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
    let policy = load("{}");
    let history = format!("{}{}", log_dump(40), log_dump(40));
    let req = req_with_history(history.clone(), "why did the deployment fail");
    match policy.transform(&req, BUDGET).await {
        TransformOutcome::Rewrite(rw) => {
            assert_eq!(rw.messages.len(), 2);
            assert_eq!(rw.messages[1]["content"], "why did the deployment fail");
            assert!(
                rw.messages[0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("ERROR")
            );
            assert!(
                rw.messages[0].get("text").is_none(),
                "reply must be BODY form (content), not projection form (text)"
            );
            assert!(
                rw.messages[0]["content"].as_str().unwrap().len() < history.len(),
                "the history must actually shrink"
            );
        }
        other => panic!("expected Rewrite, got {other:?}"),
    }
}

/// `transform` of an already-tight prompt → Abstain (nothing to compress; the original body proceeds).
#[tokio::test]
async fn transform_abstains_when_nothing_to_compress() {
    let policy = load("{}");
    let req = req_with_history("hi".to_string(), "how are you");
    assert!(matches!(
        policy.transform(&req, BUDGET).await,
        TransformOutcome::Abstain
    ));
}

/// `transform` with NO prompt projected (operator grant absent) → Abstain. The gate can never coerce
/// content: without the projection it simply has nothing to rewrite.
#[tokio::test]
async fn transform_abstains_without_grant() {
    let policy = load("{}");
    assert!(matches!(
        policy.transform(&req_without_prompt(), BUDGET).await,
        TransformOutcome::Abstain
    ));
}

/// Abstain when `min_savings_pct` is pushed to 100 via `configure` — even a real, shrinking history
/// abstains once the savings bar is unreachable.
#[tokio::test]
async fn transform_abstains_when_min_savings_unreachable() {
    let policy = load("{}");
    let mut settings = serde_json::Map::new();
    settings.insert("min_savings_pct".into(), serde_json::json!(100.0));
    policy
        .configure("headroom", &settings, 1, BUDGET)
        .await
        .expect("a well-formed configure push acks");

    let history = format!("{}{}", log_dump(40), log_dump(40));
    let req = req_with_history(history, "why did the deployment fail");
    assert!(matches!(
        policy.transform(&req, BUDGET).await,
        TransformOutcome::Abstain
    ));
}

/// `decide` abstains over the seam (the gate ranks nothing).
#[tokio::test]
async fn decide_abstains() {
    let policy = load("{}");
    let d = policy
        .decide(
            &req_with_history("hi".to_string(), "x"),
            &[cand(0)],
            &ctx(),
            BUDGET,
        )
        .await
        .expect("decide ok");
    assert_eq!(d, RoutingDecision::Abstain);
}

/// `describe` returns the settings schema + dashboard envelope; `status` reports observed settings +
/// Headroom-named metrics after a real compressing transform (tokens_saved > 0), with no prompt
/// content surfaced.
#[tokio::test]
async fn describe_and_status_over_the_seam() {
    let policy = load(r#"{"target_ratio":0.4}"#);
    let schema = policy.describe(BUDGET).await.expect("describe schema");
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["target_ratio"].is_object());

    // Run a real compressing transform so status has something honest to report.
    let history = format!("{}{}", log_dump(40), log_dump(40));
    let req = req_with_history(history, "why did the deployment fail");
    match policy.transform(&req, BUDGET).await {
        TransformOutcome::Rewrite(_) => {}
        other => panic!("expected Rewrite to drive status, got {other:?}"),
    }

    let status = policy.status(BUDGET).await.expect("status");
    let settings = status.settings.expect("status settings");
    assert_eq!(settings.get("target_ratio").unwrap(), 0.4);
    let metrics = status.metrics.expect("metrics");
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
    let policy = load("{}");
    let mut good = serde_json::Map::new();
    good.insert("target_ratio".into(), serde_json::json!(0.3));
    policy
        .configure("headroom", &good, 2, BUDGET)
        .await
        .expect("a well-formed settings push acks");

    let mut bad = serde_json::Map::new();
    bad.insert("target_ratio".into(), serde_json::json!(2.0));
    assert!(
        policy.configure("headroom", &bad, 3, BUDGET).await.is_err(),
        "an out-of-range target_ratio must NACK (malformed push does not commit)"
    );
}
