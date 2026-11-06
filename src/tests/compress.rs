// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors

//! Tests for `compress`, driving the pure `run_transform(&payload, &knobs, &metrics)` call surface
//! with the `transform` payload shape built directly (`{"request": {"pool", "messages": [...]}}`).

use super::*;

/// Test convenience: wrap knobs the way `Headroom::new` does.
fn locked(knobs: Knobs) -> RwLock<Knobs> {
    RwLock::new(knobs)
}

/// Call `run_transform` with a throwaway metrics accumulator (tests that assert metrics build
/// their own).
fn transform(payload: &Value, knobs: &RwLock<Knobs>) -> Value {
    run_transform(payload, knobs, &Mutex::new(Metrics::default()))
}

/// A synthetic log-dump line set: many segments, mostly noise, one load-bearing ERROR.
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

/// Build the `transform` payload the engine's `hooks::wire` projects: `{"request": {"pool",
/// "messages": [{"role","text"}, ...]}}`.
fn transform_payload(messages: Vec<(&str, String)>) -> Value {
    let msgs: Vec<Value> = messages
        .iter()
        .map(|(r, t)| json!({"role": r, "text": t}))
        .collect();
    json!({"request": {"pool": "p", "messages": msgs}})
}

/// A long history compresses; the reply is the rewrite arm in BODY form, ask kept verbatim.
#[test]
fn compresses_history_keeps_ask() {
    let payload = transform_payload(vec![
        ("user", log_dump(40)),
        ("assistant", log_dump(40)),
        ("user", "why did the deployment fail".to_string()),
    ]);
    let knobs = locked(Knobs {
        target_ratio: 0.4,
        min_savings_pct: 10.0,
        ..Knobs::default()
    });
    let reply = transform(&payload, &knobs);
    let msgs = reply["rewrite"]["messages"]
        .as_array()
        .expect("rewrite arm");
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[2]["content"], "why did the deployment fail");
    assert!(msgs[0]["content"].as_str().unwrap().contains("ERROR"));
    // target_ratio 0.4 keeps ~40% of tokens; assert a conservative 60% char ceiling.
    assert!(
        msgs[0]["content"].as_str().unwrap().len() < log_dump(40).len() * 6 / 10,
        "history must actually shrink"
    );
    // Body form, not projection form.
    assert!(msgs[0].get("text").is_none());
}

/// EVERY message survives the rewrite, with its own role, in its own position.
///
/// This is the load-bearing property of a hook whose reply REPLACES the request's entire messages
/// array, and nothing tested it. The only multi-message test asserts on `msgs[0]` and `msgs[2]` and
/// never looks at `msgs[1]`, so blanking every message that is neither the first nor the ask kept
/// the whole suite green. Its two history fixtures were also byte-identical, so swapping them was
/// invisible, and no test in the repo ever read an output `role`, so relabelling every assistant
/// turn as `user` reached the model unnoticed.
///
#[test]
fn every_history_message_keeps_its_content_role_and_position() {
    // Short turns bracket a long one. The long turn supplies the savings that make the rewrite
    // commit; the short ones sit under the crusher's passthrough floor, so they come back verbatim
    // and their position is checkable. Content preservation per se is NOT the property here: a
    // lossy compressor is supposed to drop low-relevance lines from a long message. What must hold
    // is that a message stays a message, in its own slot, under its own role.
    let payload = transform_payload(vec![
        ("user", "ALPHA".to_string()),
        ("assistant", log_dump(40)),
        ("user", "BRAVO".to_string()),
        ("user", "why did the deployment fail".to_string()),
    ]);
    let knobs = locked(Knobs {
        target_ratio: 0.4,
        min_savings_pct: 10.0,
        ..Knobs::default()
    });
    let reply = transform(&payload, &knobs);
    let msgs = reply["rewrite"]["messages"]
        .as_array()
        .expect("rewrite arm");

    assert_eq!(msgs.len(), 4, "no message may be added or dropped");

    // ROLES, in order. Never asserted anywhere before this.
    let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(
        roles,
        vec!["user", "assistant", "user", "user"],
        "each message must keep its own role: relabelling an assistant turn as user changes what \
         the model thinks it said"
    );

    // CONTENT, non-empty and still in its own slot. A compressor that blanked the middle, or that
    // reordered the history, fails here rather than sliding past an assertion that only looks at
    // the ends.
    for (i, m) in msgs.iter().enumerate() {
        let content = m["content"].as_str().unwrap();
        assert!(
            !content.trim().is_empty(),
            "message {i} came back empty: compression must shorten a message, never delete it"
        );
    }
    assert_eq!(
        msgs[0]["content"], "ALPHA",
        "message 0 must still be message 0"
    );
    assert_eq!(
        msgs[2]["content"], "BRAVO",
        "message 2 must still be message 2: a swap of two history turns was previously invisible, \
         because the only multi-message fixture used two byte-identical strings"
    );
    assert_eq!(msgs[3]["content"], "why did the deployment fail");
    // ...and the long turn between them really was compressed, so this is not an abstain in
    // disguise.
    assert!(
        msgs[1]["content"].as_str().unwrap().len() < log_dump(40).len(),
        "the long history turn must actually shrink"
    );
}

/// A `system` turn is passed through verbatim. On the dialects that carry the system prompt inside
/// the turns array it arrives as an ordinary message, and compressing it shreds the operator's own
/// instructions against whatever the user asked. Without this the same deployment behaves
/// differently depending on which dialect the client speaks.
#[test]
fn a_system_turn_is_never_compressed() {
    let system = format!("SYSTEM_RULES {}", log_dump(40));
    let payload = transform_payload(vec![
        ("system", system.clone()),
        ("user", log_dump(40)),
        ("user", "why did the deployment fail".to_string()),
    ]);
    let knobs = locked(Knobs {
        target_ratio: 0.4,
        min_savings_pct: 1.0,
        ..Knobs::default()
    });
    let reply = transform(&payload, &knobs);
    let msgs = reply["rewrite"]["messages"]
        .as_array()
        .expect("rewrite arm");
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(
        msgs[0]["content"].as_str().unwrap(),
        system,
        "the system turn must survive byte-for-byte"
    );
    // ...while the ordinary history turn beside it still compresses, so this is not just an
    // abstain in disguise.
    assert!(msgs[1]["content"].as_str().unwrap().len() < log_dump(40).len());
}

/// OpenAI's `developer` role carries the operator's instructions exactly as `system` does (it
/// replaces `system` for the newer models), and busbar projects it with its role intact, so it
/// must be kept verbatim on the same terms as a system turn.
#[test]
fn a_developer_turn_is_never_compressed() {
    let developer = format!("DEVELOPER_RULES {}", log_dump(40));
    let payload = transform_payload(vec![
        ("developer", developer.clone()),
        ("user", log_dump(40)),
        ("user", "why did the deployment fail".to_string()),
    ]);
    let knobs = locked(Knobs {
        target_ratio: 0.4,
        min_savings_pct: 1.0,
        ..Knobs::default()
    });
    let reply = transform(&payload, &knobs);
    let msgs = reply["rewrite"]["messages"]
        .as_array()
        .expect("rewrite arm");
    assert_eq!(msgs[0]["role"], "developer");
    assert_eq!(
        msgs[0]["content"].as_str().unwrap(),
        developer,
        "the developer turn must survive byte-for-byte"
    );
    assert!(msgs[1]["content"].as_str().unwrap().len() < log_dump(40).len());
}

/// A tool-result turn carries structure the text-only projection cannot show (`tool_call_id`, and
/// the preceding assistant turn's `tool_calls`), and busbar replaces the request's whole messages
/// array with the rewrite. Committing would ship `{"role":"tool","content":...}` with no
/// `tool_call_id` and an assistant turn with no `tool_calls`, which the provider rejects. Such a
/// prompt must abstain (and still be counted), however compressible its text is.
#[test]
fn a_prompt_with_a_tool_turn_abstains_and_is_counted() {
    for role in ["tool", "function", "Tool"] {
        let payload = transform_payload(vec![
            ("user", log_dump(40)),
            ("assistant", "{\"q\":\"deploy logs\"}".to_string()),
            (role, log_dump(40)),
            ("user", "why did the deployment fail".to_string()),
        ]);
        let knobs = locked(Knobs {
            target_ratio: 0.4,
            min_savings_pct: 1.0,
            ..Knobs::default()
        });
        let metrics = Mutex::new(Metrics::default());
        let reply = run_transform(&payload, &knobs, &metrics);
        assert_eq!(
            reply,
            json!({}),
            "a `{role}` turn must make the gate abstain"
        );
        let m = metrics.lock().unwrap();
        assert_eq!(
            m.pools["p"].requests_seen, 1,
            "the abstain is still counted"
        );
        assert_eq!(m.pools["p"].chars_in, 0, "nothing is booked as saved");
    }
}

/// A rewrite must never be committed unless it actually made the prompt shorter. `min_savings_pct`
/// is settable to 0, and the percentage is clamped at zero, so at a zero bar both "compressed
/// nothing" and "made it longer" used to satisfy the gate and commit a rewrite that achieved
/// nothing while still replacing the request's entire messages array.
#[test]
fn a_zero_savings_bar_still_requires_an_actual_reduction() {
    // Short, low-redundancy history: the crusher has nothing to remove, so chars_after >= before.
    let payload = transform_payload(vec![
        ("user", "hi".to_string()),
        ("user", "what next".to_string()),
    ]);
    let knobs = locked(Knobs {
        target_ratio: 0.4,
        min_savings_pct: 0.0,
        ..Knobs::default()
    });
    let reply = transform(&payload, &knobs);
    assert!(
        reply.get("rewrite").is_none(),
        "a zero-savings result must abstain even at a zero bar, got {reply}"
    );
}

/// The LAST message (the ask) is preserved verbatim even when it is itself long/compressible —
/// proving `run_transform`'s `last` index actually singles out the final message rather than, say,
/// running every message (including the ask) through the compressor.
#[test]
fn ask_message_is_never_compressed_even_when_compressible() {
    let ask = log_dump(40);
    let payload = transform_payload(vec![("user", log_dump(40)), ("user", ask.clone())]);
    let knobs = locked(Knobs {
        target_ratio: 0.4,
        min_savings_pct: 10.0,
        ..Knobs::default()
    });
    let reply = transform(&payload, &knobs);
    let msgs = reply["rewrite"]["messages"]
        .as_array()
        .expect("rewrite arm");
    assert_eq!(
        msgs[1]["content"].as_str().unwrap(),
        ask,
        "the ask (last message) must survive verbatim, even though it is itself compressible"
    );
}

/// Every message empty must always abstain, even at the most permissive `min_savings_pct: 0.0`
/// bar: an empty turn is a text-less turn, which is never rewritten.
#[test]
fn all_empty_messages_never_commit_even_at_zero_savings_bar() {
    let payload = transform_payload(vec![("user", String::new()), ("user", String::new())]);
    let knobs = locked(Knobs {
        min_savings_pct: 0.0,
        ..Knobs::default()
    });
    assert_eq!(
        transform(&payload, &knobs),
        json!({}),
        "zero input chars must never commit a rewrite, regardless of the savings bar"
    );
}

/// A short chat abstains (`{}`) — nothing worth compressing.
#[test]
fn short_chat_abstains() {
    let payload = transform_payload(vec![
        ("user", "hello".to_string()),
        ("assistant", "hi".to_string()),
        ("user", "how are you".to_string()),
    ]);
    let reply = transform(&payload, &locked(Knobs::default()));
    assert_eq!(reply, json!({}), "short prompts must pass through");
}

/// A shape-only payload (no prompt grant / a decide fire) abstains: no `messages` at all, and no
/// `request` key at all.
#[test]
fn no_prompt_projection_abstains() {
    let payload = json!({"request": {"pool": "p"}});
    assert_eq!(transform(&payload, &locked(Knobs::default())), json!({}));
    assert_eq!(
        transform(&json!({}), &locked(Knobs::default())),
        json!({}),
        "no request key at all (grant absent) must abstain"
    );
}

/// Garbage in -> abstain, never a panic.
#[test]
fn malformed_payload_abstains() {
    assert_eq!(
        transform(
            &json!({"request": "not an object"}),
            &locked(Knobs::default())
        ),
        json!({})
    );
    assert_eq!(
        transform(&json!(null), &locked(Knobs::default())),
        json!({})
    );
}

/// `apply_settings` is DESIRED STATE: a key absent from the push resets to the built-in default —
/// so an empty map is "back to defaults", and re-pushing the same map is a no-op.
#[test]
fn configure_is_desired_state() {
    let applied = apply_settings(&serde_json::Map::new()).expect("empty map applies cleanly");
    assert_eq!(applied.target_ratio, 0.5);
    assert_eq!(applied.min_savings_pct, 10.0);
}

/// A settings map we can't cleanly apply must return `Err` (the caller must NOT commit, keeping the
/// previous live settings): unknown key, wrong type, out-of-range value.
#[test]
fn bad_configure_never_acks_and_keeps_settings() {
    let bad_maps = [
        json!({"bogus_knob": 1}),
        json!({"target_ratio": "half"}),
        json!({"target_ratio": 2.0}),
        json!({"min_savings_pct": -1}),
        json!({"price_udollars_per_ktok": -1}),
        json!({"price_udollars_per_ktok": 1_000_001.0}),
    ];
    for v in bad_maps {
        let settings = v.as_object().unwrap();
        assert!(
            apply_settings(settings).is_err(),
            "must reject {settings:?}"
        );
    }
}

/// `price_udollars_per_ktok` at its documented boundaries (0.0 and 1_000_000.0 inclusive) applies
/// cleanly — the inverse of `bad_configure_never_acks_and_keeps_settings`'s just-outside-the-range
/// cases, pinning the `0.0..=1_000_000.0` range check to exactly the boundary it claims.
#[test]
fn price_udollars_per_ktok_boundaries_apply_cleanly() {
    for boundary in [0.0, 1_000_000.0] {
        let settings =
            serde_json::Map::from_iter([("price_udollars_per_ktok".to_string(), json!(boundary))]);
        let applied = apply_settings(&settings).expect("boundary value must apply");
        assert_eq!(applied.price_udollars_per_ktok, boundary);
    }
}

/// `describe` returns the `{schema, dashboard}` ENVELOPE (the config form + the widget layout, one
/// declaration driving both).
#[test]
fn describe_returns_schema_and_dashboard_envelope() {
    let reply = describe_reply();
    assert_eq!(reply["schema"]["type"], "object");
    assert!(reply["schema"]["properties"]["target_ratio"].is_object());
    assert!(reply["schema"]["properties"]["price_udollars_per_ktok"].is_object());
    let widgets = reply["dashboard"]["widgets"].as_array().expect("widgets");
    assert!(
        widgets
            .iter()
            .any(|w| w["metric"] == "headroom_tokens_saved_total")
    );
}

/// `status` returns observed settings + the metrics array using Headroom's REAL `/metrics` names,
/// types, and units (counters + the `headroom_overhead_ms_*` millisecond summary — no histograms,
/// matching the running proxy) plus busbar-native per-pool extras (a `pool` label, estimated $ with
/// a CI). Drives a compressing request first so the counters are non-zero.
#[test]
fn status_reports_headroom_named_metrics() {
    let knobs = locked(Knobs {
        target_ratio: 0.4,
        min_savings_pct: 10.0,
        ..Knobs::default()
    });
    let metrics = Mutex::new(Metrics::default());
    let payload = transform_payload(vec![
        ("user", log_dump(40)),
        ("assistant", log_dump(40)),
        ("user", "why did the deployment fail".to_string()),
    ]);
    assert!(
        run_transform(&payload, &knobs, &metrics)
            .get("rewrite")
            .is_some(),
        "the request must compress so counters advance"
    );

    let status = build_status(&knobs, &metrics);
    assert_eq!(status["status"]["settings"]["target_ratio"], 0.4);
    let m = status["status"]["metrics"]
        .as_array()
        .expect("metrics array");
    let by_name = |n: &str| m.iter().find(|e| e["name"] == n).cloned();

    // Headroom's REAL Prometheus names/types — the drop-in dashboard compatibility.
    let saved = by_name("headroom_tokens_saved_total").expect("headroom_tokens_saved_total");
    assert_eq!(saved["type"], "counter");
    assert!(saved["value"].as_u64().unwrap() > 0);
    assert_eq!(saved["labels"]["pool"], "p"); // transform_payload routes pool "p"
    let input_total = by_name("headroom_tokens_input_total").expect("headroom_tokens_input_total")
        ["value"]
        .as_u64()
        .unwrap();
    assert!(input_total > 0, "chars_sent must actually accumulate");
    // Input is what was SENT (post-compression), so sent + saved is what arrived: the original
    // prompt's size, give or take the per-term rounding of `est_tokens`.
    let arrived = est_tokens((2 * log_dump(40).len() + "why did the deployment fail".len()) as u64);
    let sent_plus_saved = input_total + saved["value"].as_u64().unwrap();
    assert!(
        sent_plus_saved.abs_diff(arrived) <= 1,
        "sent ({input_total}) + saved ({}) must account for the {arrived} tokens that arrived",
        saved["value"]
    );
    assert!(
        by_name("headroom_requests_total").unwrap()["value"]
            .as_u64()
            .unwrap()
            >= 1
    );

    // overhead is Headroom's real ms SUMMARY: two counters (_sum,_count) + two gauges (_min,_max).
    let sum = by_name("headroom_overhead_ms_sum").expect("overhead sum");
    assert_eq!(sum["type"], "counter");
    assert!(
        sum["value"].as_f64().unwrap() > 0.0,
        "overhead_us_sum must actually accumulate, not stay stuck at zero"
    );
    let cnt = by_name("headroom_overhead_ms_count").expect("overhead count");
    assert_eq!(cnt["type"], "counter");
    assert!(cnt["value"].as_u64().unwrap() >= 1);
    let min = by_name("headroom_overhead_ms_min").expect("overhead min");
    assert_eq!(min["type"], "gauge");
    assert!(
        min["value"].as_f64().unwrap() > 0.0,
        "overhead_us_min must be set from the first sample, not left at its zero default"
    );
    let max = by_name("headroom_overhead_ms_max").expect("overhead maximum");
    assert_eq!(max["type"], "gauge");
    assert!(
        max["value"].as_f64().unwrap() >= min["value"].as_f64().unwrap(),
        "the maximum must include the observed sample"
    );
    assert!(max["value"].as_f64().unwrap() <= sum["value"].as_f64().unwrap());
    // No histograms — the real proxy has none.
    assert!(
        m.iter().all(|e| e["type"] != "histogram"),
        "Headroom emits no histograms"
    );
    assert!(by_name("headroom_compression_ratio").is_none());
    assert!(by_name("headroom_latency_seconds").is_none());

    // busbar-native extra: estimated $ with a bounding CI.
    let dollars = by_name("dollars_saved").expect("dollars_saved");
    assert_eq!(dollars["estimated"], true);
    let (lo, val, hi) = (
        dollars["ci_low"].as_f64().unwrap(),
        dollars["value"].as_f64().unwrap(),
        dollars["ci_high"].as_f64().unwrap(),
    );
    assert!(lo <= val && val <= hi, "CI must bound the value");
}

/// A gate that has served nothing still answers `status` cleanly: observed settings + an empty
/// metrics array (busbar renders no series, fail-open).
#[test]
fn status_with_no_traffic_is_clean() {
    let knobs = locked(Knobs::default());
    let status = build_status(&knobs, &Mutex::new(Metrics::default()));
    assert!(status["status"]["metrics"].as_array().unwrap().is_empty());
    assert_eq!(status["status"]["settings"]["min_savings_pct"], 10.0);
}

/// `status.settings_version` is `null` until the FIRST `configure` push (0 is "never configured",
/// not a real version), and echoes the real number once one has landed.
#[test]
fn settings_version_is_null_until_first_configure_then_echoes() {
    let knobs = locked(Knobs::default());
    let status = build_status(&knobs, &Mutex::new(Metrics::default()));
    assert_eq!(
        status["status"]["settings_version"],
        Value::Null,
        "settings_version 0 (never configured) must render as null, not 0"
    );

    let configured = locked(Knobs {
        settings_version: 7,
        ..Knobs::default()
    });
    let status = build_status(&configured, &Mutex::new(Metrics::default()));
    assert_eq!(status["status"]["settings_version"], json!(7));
}

/// `est_tokens` rounds a char count UP to the nearest whole token at 4 chars/token — direct
/// boundary coverage independent of any timing/compression side effect.
#[test]
fn est_tokens_rounds_up_at_the_four_char_boundary() {
    assert_eq!(est_tokens(0), 0);
    assert_eq!(est_tokens(1), 1, "any remainder rounds up to a whole token");
    assert_eq!(est_tokens(4), 1, "exactly one token's worth of chars");
    assert_eq!(est_tokens(5), 2, "5 chars rounds up to 2 tokens");
    assert_eq!(est_tokens(8), 2);
}

/// `us_to_ms` is the exact `/1000.0` unit conversion the `headroom_overhead_ms_*` summary depends
/// on — a deterministic value, unlike the wall-clock samples `build_status` normally feeds it.
#[test]
fn us_to_ms_converts_microseconds_to_milliseconds() {
    assert_eq!(us_to_ms(0), 0.0);
    assert_eq!(us_to_ms(1500), 1.5);
    assert_eq!(us_to_ms(2_500_000), 2500.0);
}

/// `estimate_dollars_saved` at an exact, hand-computed value — pins the `* price / 1000.0 /
/// 1_000_000.0` formula against both `%` and `*` corruptions of either divisor, deterministically.
#[test]
fn estimate_dollars_saved_matches_hand_computed_value() {
    // 1000 tokens * 2500 micro-dollars/ktok / 1000 / 1_000_000 = 0.0025 dollars.
    assert_eq!(estimate_dollars_saved(1000, 2500.0), 0.0025);
    // A price that is NOT a round number of the divisors, so `%`/`*` corruptions of either `/`
    // produce a visibly different result than the true division: 777 * 333.0 / 1000 / 1_000_000.
    let expected = 777.0 * 333.0 / 1000.0 / 1_000_000.0;
    assert_eq!(estimate_dollars_saved(777, 333.0), expected);
    assert_eq!(estimate_dollars_saved(0, 2500.0), 0.0);
}

/// Latency: compress a realistic multi-KB history and print the wall time (feasibility datum; run
/// with `cargo test -- --nocapture latency`). Asserts a generous ceiling so CI still gates.
#[test]
fn latency_multi_kb_prompt() {
    let payload = transform_payload(vec![
        ("user", log_dump(120)),      // ~8 KB
        ("assistant", log_dump(120)), // ~8 KB
        ("user", "why did the deployment fail".to_string()),
    ]);
    let knobs = locked(Knobs::default());
    // Warm once (lazy statics inside headroom), then time.
    let _ = transform(&payload, &knobs);
    let start = std::time::Instant::now();
    let iters = 20;
    for _ in 0..iters {
        let _ = transform(&payload, &knobs);
    }
    let per = start.elapsed() / iters;
    println!("latency: {per:?} per ~16KB history compress");
    assert!(
        per < std::time::Duration::from_millis(250),
        "compression should be well under a serving-acceptable deadline, got {per:?}"
    );
}

/// The three places a setting's bounds/default are written (`apply_settings`, `settings_schema`,
/// `Knobs::default`) must agree: for every schema property the default equals the `Knobs` default,
/// `apply_settings` accepts the minimum and maximum, and rejects just outside them.
#[test]
fn settings_schema_agrees_with_knobs_and_apply_settings() {
    let schema = settings_schema();
    let props = schema["properties"].as_object().expect("properties");
    let d = Knobs::default();
    let field = |k: &Knobs, name: &str| match name {
        "target_ratio" => k.target_ratio,
        "min_savings_pct" => k.min_savings_pct,
        "price_udollars_per_ktok" => k.price_udollars_per_ktok,
        other => panic!("schema property {other:?} has no Knobs field in this test"),
    };
    assert_eq!(
        props.len(),
        3,
        "a new setting needs a row in this cross-check"
    );
    let one = |name: &str, v: f64| {
        let mut m = serde_json::Map::new();
        m.insert(name.to_string(), json!(v));
        apply_settings(&m)
    };
    for (name, p) in props {
        let (min, max, def) = (
            p["minimum"].as_f64().unwrap(),
            p["maximum"].as_f64().unwrap(),
            p["default"].as_f64().unwrap(),
        );
        assert_eq!(
            def,
            field(&d, name),
            "{name}: schema default != Knobs::default"
        );
        assert_eq!(
            field(&apply_settings(&serde_json::Map::new()).unwrap(), name),
            def
        );
        assert_eq!(
            field(&one(name, min).unwrap(), name),
            min,
            "{name}: minimum accepted"
        );
        assert_eq!(
            field(&one(name, max).unwrap(), name),
            max,
            "{name}: maximum accepted"
        );
        let eps = (max - min) * 1e-6;
        assert!(
            one(name, min - eps).is_err(),
            "{name}: just below minimum must be rejected"
        );
        assert!(
            one(name, max + eps).is_err(),
            "{name}: just above maximum must be rejected"
        );
    }
}

/// `transform_payload` with an explicit pool.
fn pool_payload(pool: &str, messages: Vec<(&str, String)>) -> Value {
    let mut p = transform_payload(messages);
    p["request"]["pool"] = json!(pool);
    p
}

/// Metrics ACCUMULATE across requests and per pool: two committing + one abstaining transform on
/// pool `p` and one abstaining on pool `q`. Requests, overhead samples and input tokens count every
/// request; tokens saved count ONLY the committed requests' savings.
#[test]
fn metrics_accumulate_per_pool_and_only_committed_saves() {
    let knobs = locked(Knobs {
        target_ratio: 0.4,
        min_savings_pct: 10.0,
        ..Knobs::default()
    });
    let metrics = Mutex::new(Metrics::default());
    let big = || {
        pool_payload(
            "p",
            vec![
                ("user", log_dump(40)),
                ("user", "why did the deployment fail".to_string()),
            ],
        )
    };
    let small = |pool: &str| {
        pool_payload(
            pool,
            vec![("user", "hi".to_string()), ("user", "there".to_string())],
        )
    };
    let mut delta_chars = 0u64;
    for _ in 0..2 {
        let reply = run_transform(&big(), &knobs, &metrics);
        let out: usize = reply["rewrite"]["messages"]
            .as_array()
            .expect("committed")
            .iter()
            .map(|m| m["content"].as_str().unwrap().len())
            .sum();
        let before = log_dump(40).len() + "why did the deployment fail".len();
        delta_chars += (before - out) as u64;
    }
    assert_eq!(run_transform(&small("p"), &knobs, &metrics), json!({}));
    assert_eq!(run_transform(&small("q"), &knobs, &metrics), json!({}));

    let status = build_status(&knobs, &metrics);
    let m = status["status"]["metrics"].as_array().unwrap();
    let val = |name: &str, pool: &str| {
        m.iter()
            .find(|e| e["name"] == name && e["labels"]["pool"] == pool)
            .unwrap_or_else(|| panic!("{name}{{pool={pool}}}"))["value"]
            .clone()
    };
    assert_eq!(val("headroom_requests_total", "p"), 3);
    assert_eq!(val("headroom_requests_total", "q"), 1);
    assert_eq!(val("headroom_overhead_ms_count", "p"), 3);
    assert_eq!(val("headroom_overhead_ms_count", "q"), 1);
    assert_eq!(
        val("headroom_tokens_saved_total", "p"),
        est_tokens(delta_chars),
        "only the committed requests' savings count"
    );
    assert_eq!(val("headroom_tokens_saved_total", "q"), 0);
}

/// Every dashboard widget's `metric` is a `name` that `build_status` actually emits.
#[test]
fn dashboard_widgets_reference_emitted_metric_names() {
    let knobs = locked(Knobs::default());
    let metrics = Mutex::new(Metrics::default());
    let payload = transform_payload(vec![
        ("user", log_dump(40)),
        ("user", "why did the deployment fail".to_string()),
    ]);
    assert!(
        run_transform(&payload, &knobs, &metrics)
            .get("rewrite")
            .is_some()
    );
    let status = build_status(&knobs, &metrics);
    let emitted: Vec<&str> = status["status"]["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    let describe = describe_reply();
    let widgets = describe["dashboard"]["widgets"].as_array().unwrap();
    assert!(!widgets.is_empty());
    for w in widgets {
        let metric = w["metric"].as_str().unwrap();
        assert!(
            emitted.contains(&metric),
            "widget metric {metric:?} is never emitted by build_status: {emitted:?}"
        );
    }
}

#[test]
fn invalid_settings_do_not_echo_arbitrary_input() {
    let secret = "private-token-do-not-log";
    for key in ["target_ratio", "min_savings_pct", "price_udollars_per_ktok"] {
        let settings = serde_json::Map::from_iter([(key.to_string(), json!(secret))]);
        let error = apply_settings(&settings).err().expect("reject wrong type");
        assert!(!error.contains(secret));
        assert!(error.contains(key));
        let settings = serde_json::Map::from_iter([(key.to_string(), json!(1234567890123456_u64))]);
        let error = apply_settings(&settings)
            .err()
            .expect("reject out-of-range value");
        assert!(!error.contains("1234567890123456"));
        assert!(error.contains(key));
    }
    let settings = serde_json::Map::from_iter([(secret.to_string(), json!(0))]);
    let error = apply_settings(&settings)
        .err()
        .expect("reject unknown setting");
    assert!(!error.contains(secret));
}

#[test]
fn single_message_and_empty_history_are_counted_without_rewriting() {
    let knobs = locked(Knobs::default());
    let metrics = Mutex::new(Metrics::default());
    let payload = transform_payload(vec![("user", "hello".to_string())]);
    assert_eq!(run_transform(&payload, &knobs, &metrics), json!({}));
    assert_eq!(
        run_transform(&transform_payload(vec![]), &knobs, &metrics),
        json!({})
    );
    let status = build_status(&knobs, &metrics);
    let series = status["status"]["metrics"].as_array().unwrap();
    let value = |name: &str| series.iter().find(|m| m["name"] == name).unwrap()["value"].clone();
    assert_eq!(value("headroom_requests_total"), 2);
    assert_eq!(value("headroom_tokens_input_total"), 2);
    assert_eq!(value("headroom_tokens_saved_total"), 0);
    assert_eq!(value("headroom_overhead_ms_count"), 2);
}

/// The overhead min/max gauges fold every sample, not just the first: after a near-instant
/// abstain and a multi-KB compress on the same pool, min is strictly below max and the sum sits
/// between `count * min` and `count * max`. A swapped `min`/`max` fold collapses min onto max.
#[test]
fn overhead_min_and_max_track_the_fastest_and_slowest_samples() {
    let knobs = locked(Knobs::default());
    let metrics = Mutex::new(Metrics::default());
    let tiny = pool_payload("p", vec![("user", "hi".to_string())]);
    let big = pool_payload(
        "p",
        vec![
            ("user", log_dump(120)),
            ("assistant", log_dump(120)),
            ("user", "why did the deployment fail".to_string()),
        ],
    );
    run_transform(&tiny, &knobs, &metrics);
    run_transform(&big, &knobs, &metrics);
    run_transform(&tiny, &knobs, &metrics);
    let m = metrics.lock().unwrap();
    let s = &m.pools["p"];
    assert_eq!(s.overhead_us_count, 3);
    assert!(
        s.overhead_us_min < s.overhead_us_max,
        "min {} must be below max {}",
        s.overhead_us_min,
        s.overhead_us_max
    );
    assert!(u128::from(s.overhead_us_min) * 3 <= s.overhead_us_sum);
    assert!(s.overhead_us_sum <= u128::from(s.overhead_us_max) * 3);
}

/// `dollars_saved` is priced off the CONFIGURED `price_udollars_per_ktok`, with the ±15% interval
/// around it, not off the default price.
#[test]
fn dollars_saved_follows_the_configured_price() {
    let knobs = locked(Knobs {
        target_ratio: 0.4,
        min_savings_pct: 1.0,
        price_udollars_per_ktok: 4000.0,
        ..Knobs::default()
    });
    let metrics = Mutex::new(Metrics::default());
    let payload = transform_payload(vec![
        ("user", log_dump(40)),
        ("user", "why did the deployment fail".to_string()),
    ]);
    assert!(
        run_transform(&payload, &knobs, &metrics)
            .get("rewrite")
            .is_some()
    );
    let status = build_status(&knobs, &metrics);
    let m = status["status"]["metrics"].as_array().unwrap();
    let find = |name: &str| m.iter().find(|e| e["name"] == name).unwrap().clone();
    let saved = find("headroom_tokens_saved_total")["value"]
        .as_u64()
        .unwrap();
    assert!(saved > 0);
    let dollars = find("dollars_saved");
    let want = saved as f64 * 4000.0 / 1000.0 / 1_000_000.0;
    assert_eq!(dollars["value"].as_f64().unwrap(), want);
    assert_eq!(dollars["ci_low"].as_f64().unwrap(), want * 0.85);
    assert_eq!(dollars["ci_high"].as_f64().unwrap(), want * 1.15);
}

/// Instruction roles are matched case-insensitively: `SYSTEM` and `Developer` are kept verbatim.
#[test]
fn instruction_roles_are_matched_case_insensitively() {
    for role in ["SYSTEM", "System", "Developer", "DEVELOPER"] {
        let rules = format!("RULES {}", log_dump(40));
        let payload = transform_payload(vec![
            (role, rules.clone()),
            ("user", log_dump(40)),
            ("user", "why did the deployment fail".to_string()),
        ]);
        let knobs = locked(Knobs {
            target_ratio: 0.4,
            min_savings_pct: 1.0,
            ..Knobs::default()
        });
        let reply = transform(&payload, &knobs);
        assert_eq!(
            reply["rewrite"]["messages"][0]["content"].as_str(),
            Some(rules.as_str()),
            "a `{role}` turn must survive byte-for-byte"
        );
    }
}

/// A payload without `request.pool` is folded into the `unknown` bucket, not dropped.
#[test]
fn a_missing_pool_is_counted_under_unknown() {
    let knobs = locked(Knobs::default());
    let metrics = Mutex::new(Metrics::default());
    let payload = json!({"request": {"messages": [{"role": "user", "text": "hi"}]}});
    assert_eq!(run_transform(&payload, &knobs, &metrics), json!({}));
    let m = metrics.lock().unwrap();
    assert_eq!(m.pools.len(), 1);
    assert_eq!(m.pools["unknown"].requests_seen, 1);
    assert_eq!(m.pools["unknown"].chars_sent, 2);
}

/// The savings bar is inclusive: savings exactly equal to `min_savings_pct` commit.
#[test]
fn savings_exactly_at_the_bar_commit() {
    let payload = transform_payload(vec![
        ("user", log_dump(40)),
        ("user", "why did the deployment fail".to_string()),
    ]);
    let probe = transform(
        &payload,
        &locked(Knobs {
            min_savings_pct: 0.0,
            ..Knobs::default()
        }),
    );
    let msgs = probe["rewrite"]["messages"]
        .as_array()
        .expect("rewrite arm");
    let after: usize = msgs
        .iter()
        .map(|m| m["content"].as_str().unwrap().len())
        .sum();
    let before = log_dump(40).len() + "why did the deployment fail".len();
    assert!(after < before);
    let pct = 100.0 * (before - after) as f64 / before as f64;
    let at_bar = transform(
        &payload,
        &locked(Knobs {
            min_savings_pct: pct,
            ..Knobs::default()
        }),
    );
    assert_eq!(at_bar, probe, "savings equal to the bar must commit");
}

/// A projection with the operator's system prompt hoisted into `request.system` (busbar 1.6.0
/// hoists every in-band system/developer turn there) and a compressible history.
fn payload_with_system(protocol: Option<&str>, system: &str) -> Value {
    let mut request = json!({
        "pool": "p",
        "system": system,
        "messages": [
            {"role": "user", "text": log_dump(40)},
            {"role": "assistant", "text": log_dump(40)},
            {"role": "user", "text": "why did the deployment fail"}
        ]
    });
    if let Some(protocol) = protocol {
        request["ingress_protocol"] = json!(protocol);
    }
    json!({ "request": request })
}

/// On dialects that carry the system prompt inside the turns array, busbar hoists it into
/// `request.system` and then REPLACES the whole array with the rewrite, so a commit would ship the
/// request without its system prompt. The gate must abstain there (and still count the request).
#[test]
fn a_hoisted_system_prompt_on_an_in_band_dialect_abstains() {
    let knobs = locked(Knobs {
        target_ratio: 0.4,
        min_savings_pct: 1.0,
        ..Knobs::default()
    });
    for protocol in [
        Some("openai"),
        Some("responses"),
        Some("cohere"),
        Some("a-new-dialect"),
        None,
    ] {
        let metrics = Mutex::new(Metrics::default());
        let reply = run_transform(
            &payload_with_system(protocol, "You are a billing bot."),
            &knobs,
            &metrics,
        );
        assert_eq!(
            reply,
            json!({}),
            "{protocol:?} must abstain rather than drop the system prompt"
        );
        assert_eq!(metrics.lock().unwrap().pools["p"].requests_seen, 1);
    }
}

/// Where the system prompt lives in its own body field (anthropic `system`, bedrock `system`,
/// gemini `systemInstruction`) the rewrite never touches it, so compression proceeds.
#[test]
fn a_system_prompt_in_its_own_body_field_still_compresses() {
    let knobs = locked(Knobs {
        target_ratio: 0.4,
        min_savings_pct: 1.0,
        ..Knobs::default()
    });
    for protocol in ["anthropic", "bedrock", "gemini"] {
        let reply = transform(
            &payload_with_system(Some(protocol), "You are a billing bot."),
            &knobs,
        );
        assert!(
            reply.get("rewrite").is_some(),
            "{protocol} keeps its system prompt out of the rewrite"
        );
    }
    // An empty system prompt is not a system prompt (busbar omits it), on any dialect.
    let reply = transform(&payload_with_system(Some("openai"), ""), &knobs);
    assert!(reply.get("rewrite").is_some());
}

/// The ask is the last turn only when it is a user turn. With an assistant prefill (or a trailing
/// instruction turn) last, compressing would shred the user's real question against the prefill,
/// so the gate abstains and still counts the request.
#[test]
fn a_prompt_that_does_not_end_on_a_user_turn_abstains() {
    let knobs = locked(Knobs {
        target_ratio: 0.4,
        min_savings_pct: 1.0,
        ..Knobs::default()
    });
    for last in ["assistant", "system", "developer"] {
        let metrics = Mutex::new(Metrics::default());
        let payload = transform_payload(vec![
            ("user", log_dump(40)),
            (last, "Here is the JSON:".to_string()),
        ]);
        assert_eq!(
            run_transform(&payload, &knobs, &metrics),
            json!({}),
            "last turn `{last}`"
        );
        assert_eq!(metrics.lock().unwrap().pools["p"].requests_seen, 1);
    }
}

/// busbar projects an image-only (or otherwise text-less) turn as empty text. A committed rewrite
/// would ship it as `{"content": ""}`, which Anthropic and Bedrock reject outright, turning a request
/// that would have succeeded into a provider error. Such a prompt abstains (and is counted).
#[test]
fn a_prompt_with_a_text_less_turn_abstains() {
    let knobs = locked(Knobs {
        target_ratio: 0.4,
        min_savings_pct: 1.0,
        ..Knobs::default()
    });
    for (empty_at, blank) in [(2usize, ""), (1, ""), (2, "  \n ")] {
        let mut turns = vec![
            ("user", log_dump(40)),
            ("assistant", log_dump(40)),
            ("user", "why did the deployment fail".to_string()),
        ];
        turns[empty_at].1 = blank.to_string();
        let metrics = Mutex::new(Metrics::default());
        let reply = run_transform(&transform_payload(turns), &knobs, &metrics);
        assert_eq!(
            reply,
            json!({}),
            "a text-less turn at {empty_at} must abstain"
        );
        assert_eq!(metrics.lock().unwrap().pools["p"].requests_seen, 1);
    }
}

/// `min_savings_pct` is a share of the WHOLE prompt. On a dialect whose system prompt survives the
/// rewrite in its own field, that system prompt is part of what ships, so it counts in the
/// denominator: a small history behind a large system prompt must not clear a 10% bar.
#[test]
fn the_savings_bar_counts_a_system_prompt_that_ships_unchanged() {
    let knobs = locked(Knobs {
        target_ratio: 0.4,
        min_savings_pct: 10.0,
        ..Knobs::default()
    });
    let big_system = "Operator rules. ".repeat(4_000); // ~64 KB vs ~10 KB of history
    let reply = transform(&payload_with_system(Some("anthropic"), &big_system), &knobs);
    assert_eq!(
        reply,
        json!({}),
        "a few percent of the whole prompt must not clear a 10% bar"
    );
    // The same history without the system prompt does clear it.
    let reply = transform(&payload_with_system(Some("anthropic"), ""), &knobs);
    assert!(reply.get("rewrite").is_some());
}

/// `headroom_tokens_input_total` follows Headroom's definition: input tokens actually sent upstream,
/// i.e. AFTER a committed rewrite, so `saved / (input + saved)` is the real reduction.
#[test]
fn input_tokens_count_what_was_sent_upstream() {
    let knobs = locked(Knobs {
        target_ratio: 0.4,
        min_savings_pct: 1.0,
        ..Knobs::default()
    });
    let metrics = Mutex::new(Metrics::default());
    let payload = transform_payload(vec![
        ("user", log_dump(40)),
        ("user", "why did the deployment fail".to_string()),
    ]);
    let reply = run_transform(&payload, &knobs, &metrics);
    let sent: usize = reply["rewrite"]["messages"]
        .as_array()
        .expect("committed")
        .iter()
        .map(|m| m["content"].as_str().unwrap().len())
        .sum();
    // An abstaining request ships unchanged, so all of it counts.
    let small = transform_payload(vec![("user", "hi".to_string())]);
    assert_eq!(run_transform(&small, &knobs, &metrics), json!({}));
    let status = build_status(&knobs, &metrics);
    let input = status["status"]["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "headroom_tokens_input_total")
        .unwrap()["value"]
        .as_u64()
        .unwrap();
    assert_eq!(input, est_tokens((sent + 2) as u64));
}

/// Over `limits.hook_content_max_bytes` busbar shows the hook an EMPTY projection but keeps the real
/// size in `request.total_chars`. That prompt still went upstream whole, so it counts as sent input.
#[test]
fn a_prompt_omitted_by_the_content_cap_counts_its_real_size() {
    let knobs = locked(Knobs::default());
    let metrics = Mutex::new(Metrics::default());
    let payload = json!({"request": {"pool": "p", "total_chars": 4_000_000, "messages": []}});
    assert_eq!(run_transform(&payload, &knobs, &metrics), json!({}));
    let m = metrics.lock().unwrap();
    assert_eq!(m.pools["p"].requests_seen, 1);
    assert_eq!(m.pools["p"].chars_sent, 4_000_000);
}

/// The ask is the BM25 query: the history segment that matches it survives a deep cut, while the
/// same history compressed against a different ask loses it.
#[test]
fn the_last_message_steers_what_the_history_keeps() {
    let mut history = String::new();
    for i in 0..40 {
        if i == 7 {
            history.push_str("The zebra cache was rebuilt from the cold snapshot overnight.\n");
        } else {
            history.push_str(&format!(
                "Routine step {i} completed in the staging environment without incident.\n"
            ));
        }
    }
    let knobs = locked(Knobs {
        target_ratio: 0.2,
        min_savings_pct: 1.0,
        ..Knobs::default()
    });
    let kept = |ask: &str| {
        let reply = transform(
            &transform_payload(vec![("user", history.clone()), ("user", ask.to_string())]),
            &knobs,
        );
        reply["rewrite"]["messages"][0]["content"]
            .as_str()
            .expect("rewrite arm")
            .to_string()
    };
    assert!(kept("what happened to the zebra cache snapshot").contains("zebra cache"));
    assert!(!kept("which staging step ran last").contains("zebra cache"));
}

/// The emitted `headroom_overhead_ms_*` series carry the pool's own min / max / sum / count, each
/// folded from microseconds to milliseconds, so a mixed-up field or a missed unit fold shows here.
#[test]
fn status_emits_each_overhead_series_from_its_own_field() {
    let knobs = locked(Knobs::default());
    let metrics = Mutex::new(Metrics::default());
    {
        let mut m = metrics.lock().unwrap();
        let s = m.pools.entry("p".to_string()).or_default();
        s.overhead_us_sum = 7_500;
        s.overhead_us_count = 3;
        s.overhead_us_min = 500;
        s.overhead_us_max = 6_000;
    }
    let status = build_status(&knobs, &metrics);
    let series = status["status"]["metrics"].as_array().unwrap();
    let value = |name: &str| series.iter().find(|e| e["name"] == name).unwrap()["value"].clone();
    assert_eq!(value("headroom_overhead_ms_sum"), json!(7.5));
    assert_eq!(value("headroom_overhead_ms_count"), json!(3));
    assert_eq!(value("headroom_overhead_ms_min"), json!(0.5));
    assert_eq!(value("headroom_overhead_ms_max"), json!(6.0));
}
