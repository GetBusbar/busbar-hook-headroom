use super::*;
use serde_json::json;

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

/// The actual current door carries the operator-facing alias and name.
#[test]
fn linked_row_states_the_shipped_manifest_name() {
    let row = busbar_plugin_loader::dispatch::LinkedRow::of(door).unwrap();
    let statement = busbar_contract::abi::mechanism::rendering::read(&row.statement).unwrap();
    assert_eq!(statement.name, "busbar-hook-headroom");
    assert!(statement.rewrites.iter().any(|r| r.1 == "headroom"));
}

/// Configure refusal carries its reason through the current ABI instead of a global log sink.
#[test]
fn rejected_configure_returns_the_reason() {
    let h = Headroom::new(Knobs::default());
    let why = h
        .configure(json!({"target_ratio": 5}).as_object().unwrap(), 4242)
        .unwrap_err();
    assert!(
        why.contains("target_ratio") && why.contains("4242"),
        "{why}"
    );
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
        h.transform_payload(&payload).get("rewrite").is_some(),
        "defaults must rewrite this history"
    );

    let mut settings = Map::new();
    settings.insert("target_ratio".into(), json!(0.4));
    settings.insert("min_savings_pct".into(), json!(100.0));
    assert!(
        h.configure(&settings, 7).is_ok(),
        "a well-formed push must ACK"
    );
    assert_eq!(h.knobs.read().unwrap().target_ratio, 0.4);
    assert_eq!(h.knobs.read().unwrap().min_savings_pct, 100.0);
    // The same instance's NEXT transform sees the pushed settings.
    assert_eq!(
        h.transform_payload(&payload),
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
        h.configure(&Map::new(), 3).is_ok(),
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
        assert!(!h.configure(settings, 9).is_ok(), "must NACK {settings:?}");
        let k = h.knobs.read().unwrap();
        assert_eq!(
            k.target_ratio, 0.3,
            "target_ratio must survive a rejected configure: {settings:?}"
        );
        assert_eq!(
            k.min_savings_pct, 25.0,
            "min_savings_pct must survive a rejected configure: {settings:?}"
        );
    }
}

/// busbar computes settings drift by comparing each desired value with the `status` echo using
/// exact JSON equality, where the integer `10` and the float `10.0` differ. Every shipped config
/// writes `min_savings_pct: 10`, so the echo must hand back the pushed value as it was pushed, or the
/// hook reports drift on every poll while running exactly what it was told.
#[test]
fn status_echoes_pushed_settings_exactly_as_pushed() {
    let h = Headroom::new(Knobs::default());
    let pushed =
        json!({"target_ratio": 1, "min_savings_pct": 10, "price_udollars_per_ktok": 2500.5});
    assert!(h.configure(pushed.as_object().unwrap(), 2).is_ok());
    let settings = h.status()["status"]["settings"].clone();
    for (key, want) in pushed.as_object().unwrap() {
        assert_eq!(
            settings.get(key),
            Some(want),
            "`{key}` must echo exactly as pushed (got {settings})"
        );
    }

    // A later push that omits keys resets them to their defaults, and the echo follows.
    assert!(
        h.configure(json!({"min_savings_pct": 20}).as_object().unwrap(), 3)
            .is_ok()
    );
    let settings = h.status()["status"]["settings"].clone();
    assert_eq!(settings["min_savings_pct"], json!(20));
    assert_eq!(settings["target_ratio"], json!(0.5));
    assert_eq!(settings["price_udollars_per_ktok"], json!(2500.0));

    // A rejected push leaves the echo on the last applied settings.
    assert!(
        !h.configure(json!({"min_savings_pct": 101}).as_object().unwrap(), 4)
            .is_ok()
    );
    assert_eq!(
        h.status()["status"]["settings"]["min_savings_pct"],
        json!(20)
    );
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

#[test]
fn malformed_open_config_does_not_echo_its_contents() {
    let error = open(r#""private-config-secret""#)
        .err()
        .expect("nonobject config is rejected");
    assert!(!error.contains("private-config-secret"));
    assert!(error.contains("invalid plugin config"));
}

#[test]
fn checkpoint_commits_only_valid_settings_and_recreates_desired_defaults() {
    let h = Headroom::new(Knobs::default());
    let first = h
        .configure_checkpoint(json!({"min_savings_pct": 20}).as_object().unwrap(), 3)
        .unwrap()
        .unwrap();
    assert_eq!((first.revision, first.settings_version), (1, 3));
    assert!(
        h.configure_checkpoint(json!({"min_savings_pct": 101}).as_object().unwrap(), 4)
            .is_err()
    );
    assert_eq!(h.status()["status"]["settings_version"], 3);
    let second = h.configure_checkpoint(&Map::new(), 5).unwrap().unwrap();
    assert_eq!((second.revision, second.settings_version), (2, 5));
    let replay = open(std::str::from_utf8(second.settings()).unwrap()).unwrap();
    assert_eq!(
        replay.status()["status"]["settings"],
        h.status()["status"]["settings"]
    );
    assert_eq!(
        h.knobs.read().unwrap().min_savings_pct,
        Knobs::default().min_savings_pct
    );
}
