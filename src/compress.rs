// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors

//! The pure half of the plugin: busbar's `transform` payload in, reply JSON out — plus the
//! `configure`/`describe`/`status` support types the [`crate::Headroom`] gate wires into
//! [`busbar_contract::abi::sdk::hook::Hook`].
//!
//! This module has NO knowledge of the dlopen ABI or the SDK trait; it is unit testable on its
//! own.
//!
//! ## Payload shape (the `transform` op)
//!
//! The engine's `hooks::wire` projection carries the granted prompt as `{"request": {"pool"?,
//! "ingress_protocol"?, "system"?, "messages"?: [{"role", "text"}]}}`. We read ONLY `text` on
//! input messages, and we emit BODY-form `{"role", "content"}` on output. That asymmetry is deliberate and
//! real, not a bug: the engine splices our reply straight into the pending request body's
//! `messages` array, which is BODY-form, not projection-form.
//!
//! Rewrite strategy — compress the HISTORY, keep the ask: the LAST message (a `user` turn) is
//! preserved verbatim and its text becomes the BM25 relevance query for every earlier message, so the parts of the
//! history that matter to the current ask survive. TextCrusher itself passes short texts (<6
//! segments) through unchanged, and we abstain outright when total savings are below
//! `min_savings_pct` — a no-win rewrite costs a body re-render for nothing.

use headroom_core::transforms::TextCrusher;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Mutex, RwLock};

/// One message of the engine's prompt projection (`{role, text}` — flattened text form).
#[derive(Deserialize)]
struct ProjMessage {
    role: String,
    text: String,
}

/// The slice of the `transform` payload's `request` this gate reads. Everything else is ignored
/// (the projection is append-only; unknown fields must never break a hook).
#[derive(Deserialize)]
struct ProjRequest {
    /// The pool this request routes through — the label dimension for every per-pool metric.
    /// Absent on an older engine / a probe folds into an `"unknown"` bucket rather than dropping
    /// the count.
    #[serde(default)]
    pool: Option<String>,
    /// The ingress dialect (`openai`, `anthropic`, ...): decides whether the system prompt rides
    /// in the turns array the rewrite replaces.
    #[serde(default)]
    ingress_protocol: Option<String>,
    /// The system prompt busbar hoisted out of the turns (absent when there is none).
    #[serde(default)]
    system: Option<String>,
    /// The request's real text size, present even when `limits.hook_content_max_bytes` made busbar
    /// omit the content and send an empty projection.
    #[serde(default)]
    total_chars: Option<u64>,
    #[serde(default)]
    messages: Option<Vec<ProjMessage>>,
}

/// The whole `transform` payload shape: `{"request": {...}}`.
#[derive(Deserialize)]
struct HookLine {
    request: ProjRequest,
}

/// Compression knobs (config-seeded at `open`, replaced live by `configure` pushes).
#[derive(Clone)]
pub struct Knobs {
    /// Fraction of characters to KEEP per compressed message (TextCrusher `target_ratio`, applied
    /// in whole sentence segments).
    pub target_ratio: f64,
    /// Abstain unless the whole-prompt byte savings reach this percentage.
    pub min_savings_pct: f64,
    /// Assumed input price in micro-dollars per 1,000 tokens, used to turn tokens-saved into the
    /// estimated `dollars_saved` metric (busbar's measured `/usage` spend is the separate truth;
    /// this is the hook's own estimate, marked `estimated` with a confidence interval). Default
    /// 2,500 ≈ $2.50 / 1M input tokens.
    pub price_udollars_per_ktok: f64,
    /// The `settings_version` of the last committed `configure`, echoed in `status` so busbar can
    /// compute version drift. 0 until the first configure.
    pub settings_version: u64,
    /// The applied settings exactly as they were pushed, echoed by `status`. busbar computes drift
    /// by exact JSON equality against the desired value, where an integer `10` and the `10.0` an
    /// `f64` re-serializes to differ, so the echo must be the pushed value, not the parsed one.
    pub pushed: serde_json::Map<String, Value>,
}

impl Default for Knobs {
    fn default() -> Self {
        Self {
            target_ratio: 0.5,
            min_savings_pct: 10.0,
            price_udollars_per_ktok: 2500.0,
            settings_version: 0,
            pushed: serde_json::Map::new(),
        }
    }
}

/// Apply a pushed settings map as DESIRED STATE: present keys override, absent keys reset to the
/// built-in defaults — so re-pushing the same map is a no-op (idempotent). FAIL-CLOSED on commit:
/// an unknown key or an out-of-shape/out-of-range value returns `Err`, the caller does NOT commit,
/// and the gate keeps its previous settings (the operator's `configure` push is NACKed instead of
/// us half-applying a map we didn't understand).
pub fn apply_settings(settings: &serde_json::Map<String, Value>) -> Result<Knobs, String> {
    let mut knobs = Knobs::default();
    for (key, value) in settings {
        match key.as_str() {
            "target_ratio" => {
                let v = value
                    .as_f64()
                    .ok_or_else(|| "target_ratio must be a number".to_string())?;
                if !(0.05..=1.0).contains(&v) {
                    return Err("target_ratio must be in 0.05..=1.0".to_string());
                }
                knobs.target_ratio = v;
            }
            "min_savings_pct" => {
                let v = value
                    .as_f64()
                    .ok_or_else(|| "min_savings_pct must be a number".to_string())?;
                if !(0.0..=100.0).contains(&v) {
                    return Err("min_savings_pct must be in 0..=100".to_string());
                }
                knobs.min_savings_pct = v;
            }
            "price_udollars_per_ktok" => {
                let v = value.as_f64().ok_or_else(|| {
                    "price_udollars_per_ktok must be a number".to_string()
                })?;
                if !(0.0..=1_000_000.0).contains(&v) {
                    return Err("price_udollars_per_ktok must be in 0..=1_000_000".to_string());
                }
                knobs.price_udollars_per_ktok = v;
            }
            _ => return Err("unknown setting; expected target_ratio, min_savings_pct, or price_udollars_per_ktok".to_string()),
        }
    }
    knobs.pushed = settings.clone();
    Ok(knobs)
}

/// The `describe` reply ENVELOPE: `{schema, dashboard}`. `schema` is the settings JSON Schema the
/// engine serves verbatim at `GET /api/v1/admin/hooks/{name}/schema` (the config form); `dashboard`
/// declares a widget layout for a plugin dashboard (busbar 1.6.0 serves only `schema` and does not
/// surface it), whose values come from `status.metrics`
/// matched by `metric` name. ONE declaration drives both.
pub fn describe_reply() -> Value {
    json!({
        "schema": settings_schema(),
        "dashboard": { "widgets": [
            {"metric": "headroom_tokens_saved_total",   "label": "Tokens saved",      "viz": "counter"},
            {"metric": "headroom_tokens_input_total",   "label": "Input tokens",      "viz": "counter"},
            {"metric": "headroom_overhead_ms_sum",      "label": "Overhead total (ms)", "viz": "counter", "unit": "ms"},
            {"metric": "dollars_saved",                 "label": "Proxy $ saved",     "viz": "number", "unit": "$"},
            {"metric": "headroom_requests_total",       "label": "Requests",          "viz": "counter"}
        ]}
    })
}

/// Our settings JSON Schema — the `schema` member of the `describe` envelope.
pub fn settings_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "headroom-hook settings",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "target_ratio": {
                "type": "number", "minimum": 0.05, "maximum": 1.0, "default": 0.5,
                "description": "Roughly the fraction of characters KEPT per compressed history message."
            },
            "min_savings_pct": {
                "type": "number", "minimum": 0.0, "maximum": 100.0, "default": 10.0,
                "description": "Abstain (pass the prompt through unmodified) unless the rewrite shortens the whole prompt by at least this percentage of its bytes."
            },
            "price_udollars_per_ktok": {
                "type": "number", "minimum": 0.0, "maximum": 1000000.0, "default": 2500.0,
                "description": "Assumed input price (micro-dollars per 1,000 tokens) used to estimate dollars saved."
            }
        }
    })
}

/// Per-pool operational tallies — the raw material for the `status` metrics array. Field names and
/// units mirror Headroom's real `/metrics` exposition so busbar re-exposes them 1:1. One instance
/// serves many pools; the engine sends `request.pool` on every transform, so we key by it.
#[derive(Default)]
struct PoolStat {
    /// Transform requests SEEN on this pool — `headroom_requests_total`.
    requests_seen: u64,
    /// Input bytes SENT upstream across every transform, after any committed rewrite —
    /// `headroom_tokens_input_total` (via `est_tokens`), Headroom's post-compression definition.
    chars_sent: u64,
    /// Lifetime input / output bytes on compressed requests — the `headroom_tokens_saved_total` base.
    chars_in: u64,
    chars_out: u64,
    /// Headroom-processing overhead per request, in whole microseconds — reduced into the
    /// `headroom_overhead_ms_*` millisecond summary (sum / count / min / max).
    overhead_us_sum: u128,
    overhead_us_count: u64,
    overhead_us_min: u64,
    overhead_us_max: u64,
}

/// Per-instance, per-pool metrics accumulator. Behind a `Mutex` (contention is trivial — one short
/// critical section per request); poison-tolerant on the request path.
#[derive(Default)]
pub struct Metrics {
    pools: std::collections::BTreeMap<String, PoolStat>,
}

/// Estimated tokens for a byte count (≈ bytes/4, the standard English chars/4 heuristic, which
/// over-reports on non-ASCII text). This is the hook's
/// OWN estimate for `tokens_saved` / `dollars_saved`; busbar's `/usage` reports the measured truth.
fn est_tokens(chars: u64) -> u64 {
    chars.div_ceil(4)
}

/// Whole microseconds folded to fractional milliseconds, for the `headroom_overhead_ms_*` summary.
/// Pulled out of `build_status` so the unit conversion is directly unit-testable without needing a
/// real (non-deterministic) wall-clock sample.
fn us_to_ms(us: u128) -> f64 {
    us as f64 / 1000.0
}

/// Estimated dollars saved from tokens saved, priced at `price_udollars_per_ktok` (micro-dollars
/// per 1,000 tokens). Pulled out of `build_status` so the pricing arithmetic is directly
/// unit-testable with exact expected values instead of only exercised indirectly via `status`.
fn estimate_dollars_saved(tokens_saved: u64, price_udollars_per_ktok: f64) -> f64 {
    tokens_saved as f64 * price_udollars_per_ktok / 1000.0 / 1_000_000.0
}

/// Whether a turn carries operator instructions and so is never compressed: `system`, and OpenAI's
/// `developer` role, which replaces `system` on the newer models and is projected with its role
/// intact.
fn is_instruction_role(role: &str) -> bool {
    role.eq_ignore_ascii_case("system") || role.eq_ignore_ascii_case("developer")
}

/// Whether a turn is a tool/function result. busbar projects OpenAI chat `tool`/`function`,
/// Cohere `tool` and Responses `function_call_output` as `tool`, and an unparsed Gemini body passes
/// its `function` role through.
fn is_tool_role(role: &str) -> bool {
    role.eq_ignore_ascii_case("tool") || role.eq_ignore_ascii_case("function")
}

/// Whether an ingress dialect keeps its system prompt in a body field of its own (anthropic and
/// bedrock `system`, gemini `systemInstruction`), which the rewrite never touches. Every other
/// dialect, including one this build does not know, carries it in the turns array the rewrite
/// replaces.
fn keeps_system_out_of_turns(protocol: Option<&str>) -> bool {
    matches!(protocol, Some("anthropic" | "bedrock" | "gemini"))
}

/// Run one per-message compression under PANIC CONTAINMENT: if `compress` panics, the message is
/// kept verbatim. The compressor is a parameter so the containment is testable without a
/// `TextCrusher` input that panics. (`AssertUnwindSafe` is sound here: the closure only reads
/// `text`/`query` and the compressor is dropped either way.)
pub(crate) fn compress_or_keep(
    text: &str,
    query: &str,
    target_ratio: f64,
    compress: impl Fn(&str, &str, f64) -> String,
) -> String {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        compress(text, query, target_ratio)
    }))
    .unwrap_or_else(|_| text.to_string())
}

/// The `transform` op: compress the granted prompt's history, keep the ask. Returns `{}` (abstain)
/// whenever there is no grant, no/too-short history, or the savings don't clear `min_savings_pct`;
/// otherwise `{"rewrite": {"messages": [...]}}` with each entry in BODY form
/// (`{"role", "content"}`), spliced verbatim into the pending request by the engine.
pub fn run_transform(payload: &Value, knobs: &RwLock<Knobs>, metrics: &Mutex<Metrics>) -> Value {
    let abstain = json!({});
    // Not our shape / no prompt projection (e.g. a decide fire, or a grant misconfig) -> abstain.
    let Ok(parsed) = HookLine::deserialize(payload) else {
        return abstain;
    };
    let pool = parsed.request.pool.unwrap_or_else(|| "unknown".to_string());
    let system_bytes = parsed.request.system.as_deref().map_or(0, str::len);
    let total_chars = parsed.request.total_chars.unwrap_or(0);
    let drops_system =
        system_bytes > 0 && !keeps_system_out_of_turns(parsed.request.ingress_protocol.as_deref());
    let Some(messages) = parsed.request.messages else {
        return abstain;
    };
    let (target_ratio, min_savings_pct) = {
        let k = knobs.read().unwrap_or_else(|e| e.into_inner());
        (k.target_ratio, k.min_savings_pct)
    };
    let started = std::time::Instant::now();
    let query = messages.last().map(|m| m.text.as_str()).unwrap_or_default();
    let crusher = TextCrusher::default();

    // A system prompt that survives the rewrite in its own body field still ships, so it is part
    // of the whole prompt the savings bar and the input-token count are measured against.
    let mut chars_before = system_bytes;
    let mut chars_after = system_bytes;
    let mut out: Vec<Value> = Vec::with_capacity(messages.len());
    let last = messages.len().saturating_sub(1);
    // A tool-result turn carries structure the text-only projection cannot show (`tool_call_id`,
    // and the `tool_calls` of the assistant turn it answers), and the engine replaces the request's
    // WHOLE messages array with our reply. A rewrite here would ship a tool turn with no
    // `tool_call_id` and an assistant turn with no `tool_calls`, which the provider rejects. Such a
    // prompt is counted like any other, but never compressed or rewritten.
    //
    // The same holds for a system prompt busbar hoisted out of the turns (`request.system`) on a
    // dialect whose system prompt rides IN the turns array: the rewrite replaces that array, so a
    // commit would ship the request without it. And the last turn is the ask only when it is a
    // user turn: with an assistant prefill (or a trailing instruction turn) last, compressing would
    // shred the user's real question against the prefill text.
    let has_tool_turn = messages.iter().any(|m| is_tool_role(&m.role));
    let ends_on_user = messages
        .last()
        .is_some_and(|m| m.role.eq_ignore_ascii_case("user"));
    // busbar projects an image-only (or otherwise text-less) turn as empty text; the rewrite would
    // ship it as empty content, which Anthropic and Bedrock reject.
    let has_text_less_turn = messages.iter().any(|m| m.text.trim().is_empty());
    let rewritable = !has_tool_turn && !drops_system && ends_on_user && !has_text_less_turn;

    // Fast path: when the request can never be rewritten (tool turn, hoisted system on an
    // in-band dialect, non-user ending, or a text-less turn), skip the per-message clone +
    // json! construction — only sum the byte counts for metrics and return immediately.
    if !rewritable {
        for m in &messages {
            chars_before += m.text.len();
        }
        let elapsed_us = started.elapsed().as_micros() as u64;
        {
            let mut m = metrics.lock().unwrap_or_else(|e| e.into_inner());
            let s = m.pools.entry(pool).or_default();
            s.requests_seen += 1;
            let omitted = messages.is_empty() && total_chars > 0;
            s.chars_sent += if omitted {
                total_chars
            } else {
                chars_before as u64
            };
            s.overhead_us_sum += elapsed_us as u128;
            s.overhead_us_min = if s.overhead_us_count == 0 {
                elapsed_us
            } else {
                s.overhead_us_min.min(elapsed_us)
            };
            s.overhead_us_max = s.overhead_us_max.max(elapsed_us);
            s.overhead_us_count += 1;
        }
        return abstain;
    }

    for (i, m) in messages.iter().enumerate() {
        chars_before += m.text.len();
        let text = if !rewritable || i == last || is_instruction_role(&m.role) {
            // The ask is kept verbatim, and so is an instruction (`system`/`developer`) turn that
            // arrives among the turns (an engine that projects in-band system turns in place, or
            // a mid-conversation developer turn): compressing it would shred the operator's own
            // instructions against whatever the user happened to ask.
            m.text.clone()
        } else {
            // PANIC CONTAINMENT: `TextCrusher` is third-party code on the request path. If it
            // panics on some input, keep that message verbatim instead of dying — a hook must
            // never crash on malformed/adversarial content.
            let compressed = compress_or_keep(&m.text, query, target_ratio, |text, q, ratio| {
                crusher.compress(text, q, Some(ratio)).compressed
            });
            // Guard: if the compressor returned an empty result for non-empty input, keep
            // the original — an empty `content` in the rewrite causes providers (Anthropic,
            // Bedrock) to reject the request outright, turning a working prompt into a hard
            // failure.
            if compressed.trim().is_empty() && !m.text.trim().is_empty() {
                m.text.clone()
            } else {
                compressed
            }
        };
        chars_after += text.len();
        // BODY form: {role, content} — spliced verbatim into the request's `messages`.
        out.push(json!({"role": m.role, "content": text}));
    }
    let elapsed_us = started.elapsed().as_micros() as u64;

    let savings_pct = if chars_before == 0 {
        0.0
    } else {
        100.0 * (chars_before.saturating_sub(chars_after)) as f64 / chars_before as f64
    };
    // `chars_after < chars_before` is not implied by the percentage test: `min_savings_pct` is
    // settable to 0.0, and `saturating_sub` clamps a NEGATIVE saving to 0.0, so at a zero bar both
    // "compressed nothing" and "made it longer" satisfy `savings_pct >= 0.0`. Committing there
    // reports a rewrite that achieved nothing while still paying its full cost: the engine replaces
    // the request's whole messages array with this reply. Require an actual reduction.
    let committed = rewritable && chars_after < chars_before && savings_pct >= min_savings_pct;

    // Record metrics for this request (poison-tolerant; one short critical section).
    {
        let mut m = metrics.lock().unwrap_or_else(|e| e.into_inner());
        let s = m.pools.entry(pool).or_default();
        s.requests_seen += 1;
        // An empty projection with a non-zero `total_chars` is content busbar omitted over its
        // content cap: that prompt still went upstream whole, so count its real size. busbar
        // reports it in Unicode chars, not bytes, so on non-ASCII text this undercounts.
        let omitted = messages.is_empty() && total_chars > 0;
        s.chars_sent += if committed {
            chars_after as u64
        } else if omitted {
            total_chars
        } else {
            chars_before as u64
        };
        // Fold this request's processing time into the overhead summary (µs; rendered as ms).
        s.overhead_us_sum += elapsed_us as u128;
        s.overhead_us_min = if s.overhead_us_count == 0 {
            elapsed_us
        } else {
            s.overhead_us_min.min(elapsed_us)
        };
        s.overhead_us_max = s.overhead_us_max.max(elapsed_us);
        s.overhead_us_count += 1;
        if committed {
            s.chars_in += chars_before as u64;
            s.chars_out += chars_after as u64;
        }
    }

    if committed {
        json!({"rewrite": {"messages": out}})
    } else {
        abstain
    }
}

/// The `status` settings echo: every knob's live value, with each pushed key exactly as pushed.
fn observed_settings(k: &Knobs) -> Value {
    let mut settings = serde_json::Map::new();
    settings.insert("target_ratio".into(), json!(k.target_ratio));
    settings.insert("min_savings_pct".into(), json!(k.min_savings_pct));
    settings.insert(
        "price_udollars_per_ktok".into(),
        json!(k.price_udollars_per_ktok),
    );
    for (key, value) in &k.pushed {
        settings.insert(key.clone(), value.clone());
    }
    Value::Object(settings)
}

/// Build the `status` reply: OBSERVED settings + the metrics ARRAY. Metric NAMES, TYPES, and UNITS
/// mirror Headroom's REAL `/metrics` exposition verbatim (verified against the running proxy):
/// `headroom_requests_total`, `headroom_tokens_input_total`, `headroom_tokens_saved_total` are
/// counters, and `headroom_overhead_ms_{sum,count,min,max}` is the millisecond overhead summary
/// Headroom uses (two counters + two gauges — Headroom has no native histograms). So when busbar
/// re-exposes these on its Prometheus scrape, a Grafana dashboard built for Headroom reads them off
/// busbar with no query change. Plus one busbar-native extra (`dollars_saved`, estimated + CI).
/// busbar bounds/sanitizes the array on receipt.
pub fn build_status(knobs: &RwLock<Knobs>, metrics: &Mutex<Metrics>) -> Value {
    let k = knobs.read().unwrap_or_else(|e| e.into_inner()).clone();
    let m = metrics.lock().unwrap_or_else(|e| e.into_inner());
    let mut out: Vec<Value> = Vec::new();
    for (pool, s) in m.pools.iter() {
        let tokens_saved = est_tokens(s.chars_in.saturating_sub(s.chars_out));
        out.push(json!({
            "name": "headroom_requests_total", "type": "counter", "value": s.requests_seen,
            "labels": {"pool": pool}, "label": "Requests", "viz": "counter",
            "help": "Total number of requests"
        }));
        out.push(json!({
            "name": "headroom_tokens_input_total", "type": "counter",
            "value": est_tokens(s.chars_sent), "labels": {"pool": pool}, "label": "Input tokens",
            "viz": "counter", "help": "Total input tokens"
        }));
        out.push(json!({
            "name": "headroom_tokens_saved_total", "type": "counter", "value": tokens_saved,
            "labels": {"pool": pool}, "label": "Tokens saved", "viz": "counter",
            "help": "Tokens saved by optimization"
        }));
        // headroom_overhead_ms_* — Headroom's real millisecond overhead SUMMARY (not a histogram):
        // two counters (_sum, _count) + two gauges (_min, _max), µs folded to ms to match the unit.
        // Every pool here has at least one sample: an entry is only created by a transform,
        // which records its overhead in the same critical section.
        out.push(json!({
            "name": "headroom_overhead_ms_sum", "type": "counter", "value": us_to_ms(s.overhead_us_sum),
            "labels": {"pool": pool}, "label": "Overhead total (ms)", "unit": "ms", "viz": "counter",
            "help": "Sum of Headroom processing overhead in milliseconds"
        }));
        out.push(json!({
            "name": "headroom_overhead_ms_count", "type": "counter", "value": s.overhead_us_count,
            "labels": {"pool": pool}, "label": "Overhead (count)", "viz": "counter",
            "help": "Count of observed Headroom overhead samples"
        }));
        out.push(json!({
            "name": "headroom_overhead_ms_min", "type": "gauge",
            "value": us_to_ms(s.overhead_us_min as u128), "labels": {"pool": pool},
            "label": "Overhead (min)", "unit": "ms", "viz": "number",
            "help": "Minimum observed Headroom overhead in milliseconds"
        }));
        out.push(json!({
            "name": "headroom_overhead_ms_max", "type": "gauge",
            "value": us_to_ms(s.overhead_us_max as u128), "labels": {"pool": pool},
            "label": "Overhead (max)", "unit": "ms", "viz": "number",
            "help": "Maximum observed Headroom overhead in milliseconds"
        }));
        // busbar-native extra (non-conflicting name): estimated $ saved with a confidence interval.
        let dollars = estimate_dollars_saved(tokens_saved, k.price_udollars_per_ktok);
        out.push(json!({
            "name": "dollars_saved", "type": "gauge", "value": dollars,
            "labels": {"pool": pool}, "label": "Proxy $ saved", "unit": "$", "viz": "number",
            "estimated": true, "ci_low": dollars * 0.85, "ci_high": dollars * 1.15,
            "help": "estimated input cost saved, priced from price_udollars_per_ktok (±15% CI)"
        }));
    }
    let sv = if k.settings_version > 0 {
        json!(k.settings_version)
    } else {
        Value::Null
    };
    json!({"status": {
        "settings_version": sv,
        "settings": observed_settings(&k),
        "metrics": out
    }})
}

#[cfg(test)]
#[path = "tests/compress.rs"]
mod tests;
