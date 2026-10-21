use super::*;
use std::sync::Arc;

/// `open` accepts empty/missing config and a valid settings map; a malformed config or an
/// out-of-range/unknown-key settings map fails closed.
#[test]
fn open_config_handling() {
    assert!(open("").is_ok(), "empty config uses defaults");
    assert!(open("{}").is_ok(), "no settings uses defaults");
    assert!(open(r#"{"target_ratio":0.3}"#).is_ok());
    assert!(open("{ not json").is_err(), "malformed config fails closed");
    assert!(
        open(r#"{"target_ratio":2.0}"#).is_err(),
        "out-of-range value fails closed"
    );
    assert!(
        open(r#"{"bogus_knob":1}"#).is_err(),
        "unknown key fails closed"
    );
}

/// The Statement names the plugin as the shipped tarball's manifest does (`release.yml`
/// `manifest_name`), so config `module: busbar-hook-headroom` resolves on a build that links this
/// crate.
#[test]
fn the_statement_names_the_shipped_manifest() {
    assert_eq!(NAME, "busbar-hook-headroom");
}

/// What a capturing subscriber received: `(level, message)`.
type Captured = Arc<Mutex<Vec<(tracing::Level, String)>>>;

/// A `tracing` subscriber that keeps every event's level and message.
struct Capture(Captured);

struct Message(String);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut m = Message(String::new());
        event.record(&mut m);
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((*event.metadata().level(), m.0));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// A rejected `configure` NACKs AND tells the operator why: one WARN record naming the offending
/// setting and the validation reason.
#[test]
fn rejected_configure_logs_the_reason() {
    let got: Captured = Arc::default();
    let h = Headroom::new(Knobs::default());
    let bad = json!({"target_ratio": 5});
    let acked = tracing::subscriber::with_default(Capture(Arc::clone(&got)), || {
        h.configure(bad.as_object().unwrap(), 4242)
    });
    assert!(!acked);
    let got = got.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let rec: Vec<_> = got
        .iter()
        .filter(|(_, m)| m.contains("configure v4242"))
        .collect();
    assert_eq!(
        rec.len(),
        1,
        "exactly one record for the rejected push: {got:?}"
    );
    assert_eq!(rec[0].0, tracing::Level::WARN);
    assert!(rec[0].1.contains("target_ratio"), "{}", rec[0].1);
}

/// `describe` returns the settings schema + dashboard envelope; `status` reports observed settings +
/// Headroom-named metrics after a real compressing transform (tokens_saved > 0).
#[test]
fn describe_and_status_report_a_compressing_transform() {
    let h = Headroom::new(Knobs::default());
    let described = h.describe();
    let schema = &described["schema"];
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["target_ratio"].is_object());

    let mut history = String::new();
    for i in 0..80 {
        if i == 40 {
            history.push_str("ERROR: deployment canary failed with status 503 on us-east-1.\n");
        } else {
            history.push_str(&format!(
                "Routine step {i} completed in the staging environment without incident.\n"
            ));
        }
    }
    let payload = json!({
        "request": {
            "pool": "p",
            "messages": [
                {"role": "user", "text": history},
                {"role": "user", "text": "why did the deployment fail"}
            ]
        }
    });
    assert!(h.transform(&payload).get("rewrite").is_some());

    let reply = h.status();
    let status = &reply["status"];
    assert_eq!(status["settings"]["target_ratio"], 0.5);
    let saved = status["metrics"]
        .as_array()
        .expect("metrics")
        .iter()
        .find(|m| m["name"] == "headroom_tokens_saved_total")
        .expect("headroom_tokens_saved_total metric");
    assert!(
        saved["value"].as_u64().unwrap() > 0,
        "compression saved tokens"
    );
}

/// `decide` and `notify` are inert (Headroom is not a router or a tap).
#[test]
fn decide_and_notify_are_inert() {
    let h = Headroom::new(Knobs::default());
    assert_eq!(h.decide(&json!({})), json!({}));
    h.notify(&json!({"anything": true})); // no panic, no effect
}

/// `configure` applies pushed settings LIVE and returns `true`/ACK on a clean apply; the next
/// `transform` on the SAME instance sees the pushed settings. Behavior proof: a history that
/// rewrites under the defaults abstains once `min_savings_pct: 100` is pushed.
#[test]
fn configure_applies_settings_live() {
    let h = Headroom::new(Knobs::default());
    let payload = json!({
        "request": {
            "pool": "p",
            "messages": [
                {"role": "user", "text": "Routine step completed without incident.\n".repeat(200)},
                {"role": "user", "text": "why did the deployment fail"}
            ]
        }
    });
    assert!(
        h.transform(&payload).get("rewrite").is_some(),
        "defaults must rewrite this history"
    );

    let mut settings = Map::new();
    settings.insert("target_ratio".into(), json!(0.4));
    settings.insert("min_savings_pct".into(), json!(100.0));
    assert!(h.configure(&settings, 7), "a well-formed push must ACK");
    assert_eq!(h.knobs.read().unwrap().target_ratio, 0.4);
    assert_eq!(h.knobs.read().unwrap().min_savings_pct, 100.0);
    // The same instance's NEXT transform sees the pushed settings.
    assert_eq!(
        h.transform(&payload),
        json!({}),
        "min_savings_pct 100 must abstain"
    );
}

/// `configure` is DESIRED STATE: a key absent from the push resets to the built-in default.
#[test]
fn configure_is_desired_state() {
    let h = Headroom::new(Knobs {
        target_ratio: 0.2,
        min_savings_pct: 90.0,
        ..Knobs::default()
    });
    assert!(
        h.configure(&Map::new(), 3),
        "empty map (back to defaults) must ACK"
    );
    assert_eq!(h.knobs.read().unwrap().target_ratio, 0.5);
    assert_eq!(h.knobs.read().unwrap().min_savings_pct, 10.0);
}

/// A settings push we can't cleanly apply must NACK (`false`) and leave the live knobs
/// untouched: unknown key, wrong type, out-of-range value.
#[test]
fn bad_configure_never_acks_and_keeps_settings() {
    let bad_maps = [
        json!({"bogus_knob": 1}),
        json!({"target_ratio": "half"}),
        json!({"target_ratio": 2.0}),
        json!({"min_savings_pct": -1}),
    ];
    for v in bad_maps {
        let h = Headroom::new(Knobs {
            target_ratio: 0.3,
            min_savings_pct: 25.0,
            ..Knobs::default()
        });
        let settings = v.as_object().unwrap();
        assert!(!h.configure(settings, 9), "must NACK {settings:?}");
        assert_eq!(
            h.knobs.read().unwrap().target_ratio,
            0.3,
            "settings must survive a rejected configure: {settings:?}"
        );
    }
}

/// `status` surfaces the `headroom-core` build ref alongside the ported settings/metrics shape.
#[test]
fn status_surfaces_headroom_core_ref() {
    let h = Headroom::new(Knobs::default());
    let status = h.status();
    assert_eq!(status["status"]["headroom_core_ref"], HEADROOM_CORE_REF);
    // ...and the constant is a REAL ref, not the silent-failure sentinel. Comparing the reported
    // value against the constant that produced it cannot fail on its own: it passes just as
    // happily on "unknown", which is what a build that could not read its own Cargo.lock stamps
    // in. This is the assertion that notices.
    assert_ne!(
        HEADROOM_CORE_REF, "unknown",
        "the build could not determine the headroom-core git ref, so status reports build \
         provenance that is indistinguishable from genuinely unpinned"
    );
    assert!(
        !HEADROOM_CORE_REF.trim().is_empty(),
        "headroom_core_ref must carry something an operator can act on"
    );
}

/// PANIC CONTAINMENT. The vendored `headroom-core` compress path has no `unwrap()`/`expect(`/
/// `panic!`/raw indexing, so there is no input that makes `TextCrusher` itself panic. This drives
/// the real containment function `compress::run_transform` uses per message
/// (`compress::compress_or_keep`) with a compressor that deliberately panics, proving a panicking
/// compress call degrades to the verbatim message instead of propagating, and that a healthy one
/// passes through.
#[test]
fn panic_containment_degrades_to_verbatim() {
    let kept = compress::compress_or_keep("original turn", "ask", 0.5, |_, _, _| {
        panic!("simulated TextCrusher panic")
    });
    assert_eq!(
        kept, "original turn",
        "a panicking compress call must degrade to the verbatim message, never propagate"
    );
    let ok = compress::compress_or_keep("original turn", "ask", 0.5, |t, q, r| {
        format!("{t}|{q}|{r}")
    });
    assert_eq!(ok, "original turn|ask|0.5");
}
