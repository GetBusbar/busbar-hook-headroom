// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors

//! **ONE COMPRESSION GATE, BOTH DOORS, ONE ROW** — headroom's linked + dropped-in conformance, run
//! against the busbar rev this repo pins (`.busbar-ref`).
//!
//! The gate is held two ways at once: LINKED (its `linked::HOOK` row and `BUSBAR_COLD_ENTRY`
//! boundary, what a busbar build that compiles this crate in registers) and DROPPED IN (this crate's
//! built cdylib, signed first-party under the SAME statement into a temp `plugins/` directory and
//! found by the loader's scan). Each arm is opened by the loader's one `open_hook` and driven through
//! the same scenario — describe, the transform pass over a compressible
//! history, a history below the savings bar, a request with no granted prompt and a MALFORMED
//! projection, then a configure ACK and NACK and status — and the two transcripts must agree byte for byte.
//!
//! The RED arm is in the same test: (a) the same cdylib signed as `kind: store` is refused at the
//! kind handshake naming both kinds, and (b) the same cdylib opened with a different config (a
//! savings bar nothing can clear) yields a transcript that differs from the linked one — so the
//! comparison is not vacuous.

use busbar_contract::hooks::{
    HookStatus, PromptProjection, RoutingDecision, RoutingRequest, TransformOutcome,
};
use busbar_plugin_loader::hook::HookProjectors;
use busbar_plugin_loader::sign::{HookNeeds, Manifest, NeedLevel, SigningKey, TrustPolicy, sign};
use busbar_plugin_loader::{LinkedPlugin, PluginRegistry};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const BUDGET: Duration = Duration::from_secs(5);

/// The version both arms state (a linked row states its binary's version; here, this crate's).
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The release key the dropped-in arm is signed with, and the policy's first-party key.
fn release() -> SigningKey {
    SigningKey::from_bytes(&[17u8; 32])
}

/// This crate's built cdylib (uplifted or under `deps`, newest wins). A missing artifact is a
/// failure, never a skip: this test IS the dropped-in door's proof.
fn cdylib() -> Vec<u8> {
    let exe = std::env::current_exe().expect("the test binary has a path");
    let profile = exe
        .parent()
        .and_then(|d| d.parent())
        .expect("target/<profile>");
    let file = busbar_plugin_loader::plugin_library_filename("busbar_hook_headroom");
    let found = [profile.join(&file), profile.join("deps").join(&file)]
        .into_iter()
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .max()
        .map(|(_, p)| p)
        .unwrap_or_else(|| panic!("the headroom-hook cdylib ({file}) is not built"));
    std::fs::read(found).expect("read the cdylib")
}

/// The statement both doors make: headroom as a `kind: hook` plugin declaring `prompt: rw`.
fn statement(kind: &str) -> Manifest {
    let (name, alias, _) = busbar_hook_headroom::linked::HOOK;
    let abi = busbar_plugin_loader::supported_abi(kind)
        .iter()
        .copied()
        .max()
        .unwrap_or_default();
    Manifest {
        name: name.into(),
        alias: alias.into(),
        kind: kind.into(),
        version: VERSION.into(),
        publisher: busbar_plugin_loader::sign::FIRST_PARTY_PUBLISHER.into(),
        abi_version: abi,
        sha256: String::new(),
        signature: String::new(),
        description: String::new(),
        homepage: String::new(),
        license: String::new(),
        needs: HookNeeds {
            prompt: NeedLevel::Rw,
            user: NeedLevel::No,
        },
        settings_schema: None,
        schema_derived: false,
        host: None,
        declares: Default::default(),
    }
}

/// The LINKED row: exactly what a busbar composition root that links this crate states.
fn linked_registry() -> PluginRegistry {
    let (_, _, entry) = busbar_hook_headroom::linked::HOOK;
    PluginRegistry::empty()
        .link(vec![LinkedPlugin::boundary(statement("hook"), entry)])
        .expect("the linked row registers")
}

/// THE DROPPED-IN DOOR: `lib` signed first-party under `manifest` into a fresh `plugins/`
/// directory, scanned under a policy holding the release key.
fn dropped(tag: &str, manifest: Manifest, lib: &[u8]) -> PluginRegistry {
    let dir = std::env::temp_dir().join(format!("headroom-conf-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let signed = sign(&release(), manifest, lib);
    let tarball = busbar_plugin_loader::tarball::package(&signed, "libheadroom.so", lib).unwrap();
    std::fs::write(dir.join("headroom.tar.gz"), tarball).unwrap();
    let policy = TrustPolicy {
        first_party_key: Some(release().verifying_key()),
        binary_version: VERSION.into(),
        first_party_floors: Default::default(),
        first_party_high_water: Default::default(),
        publishers: Default::default(),
        allow_unsigned: false,
        allow_third_party: false,
        min_versions: Default::default(),
    };
    busbar_plugin_loader::scan_and_validate(&dir, &policy).expect("the signed gate scans")
}

/// The engine-side projectors (fail-closed shims standing in for the engine's `hooks::wire`). The
/// `transform` projector carries the granted prompt as `request.system` + `request.messages`; for
/// the pool named `malformed` it hands the gate a MALFORMED projection (`messages` not an array).
fn projectors() -> Arc<HookProjectors> {
    Arc::new(HookProjectors {
        decide: Box::new(|req, cands, _ctx| {
            json!({
                "request": { "pool": req.pool },
                "candidates": cands.iter().map(|c| json!({"idx": c.idx})).collect::<Vec<_>>(),
            })
        }),
        transform: Box::new(|req| {
            if req.pool == "malformed" {
                return json!({"request": {"pool": req.pool, "messages": "not-an-array"}});
            }
            json!({
                "request": {
                    "pool": req.pool,
                    "system": req.prompt.as_ref().and_then(|p| p.system.as_ref().map(|s| s.as_ref().to_string())),
                    "messages": req.prompt.as_ref().map(|p| {
                        p.messages.iter().map(|(r, t)| {
                            json!({"role": r.as_ref(), "text": t.as_ref()})
                        }).collect::<Vec<_>>()
                    }),
                }
            })
        }),
        normalize: Box::new(|v, cands| {
            let Some(order) = v.get("order").and_then(|o| o.as_array()) else {
                return Ok(RoutingDecision::Abstain);
            };
            let valid: std::collections::HashSet<usize> = cands.iter().map(|c| c.idx).collect();
            Ok(RoutingDecision::from_ranked(
                order.iter().filter_map(|x| x.as_u64().map(|x| x as usize)),
                &valid,
            ))
        }),
        transform_outcome: Box::new(|v| {
            match v
                .get("rewrite")
                .and_then(|r| r.get("messages"))
                .and_then(|m| m.as_array())
            {
                Some(msgs) if !msgs.is_empty() => {
                    TransformOutcome::Rewrite(busbar_contract::hooks::RewriteReply {
                        messages: msgs.clone(),
                        tools: Vec::new(),
                    })
                }
                _ => TransformOutcome::Abstain,
            }
        }),
        status: Box::new(|v| {
            v.get("status").map(|s| HookStatus {
                settings_version: s.get("settings_version").and_then(|x| x.as_u64()),
                settings: s.get("settings").and_then(|x| x.as_object()).cloned(),
                metrics: s.get("metrics").and_then(|m| m.as_array()).cloned(),
            })
        }),
        describe_schema: Box::new(|v| v.get("schema").cloned()),
    })
}

/// A synthetic log dump: many segments, mostly noise, one load-bearing ERROR (BM25 needs a real
/// multi-segment body; TextCrusher passes short texts through unchanged).
fn log_dump(lines: usize) -> String {
    (0..lines)
        .map(|i| match i == lines / 2 {
            true => "ERROR: deployment canary failed with status 503 on us-east-1.\n".to_string(),
            false => {
                format!("Routine step {i} completed in the staging environment without incident.\n")
            }
        })
        .collect()
}

/// A request on `pool` carrying `messages` as the granted prompt (`None` = grant absent).
fn request(pool: &'static str, messages: Option<Vec<String>>) -> RoutingRequest<'static> {
    let total_chars = messages
        .as_ref()
        .map_or(0, |m| m.iter().map(String::len).sum());
    RoutingRequest {
        request_id: 7,
        pool,
        ingress_protocol: "anthropic",
        requested_model: None,
        message_count: messages.as_ref().map_or(0, Vec::len),
        tool_count: 0,
        has_tools: false,
        total_chars,
        system_chars: 0,
        max_tokens: None,
        stream: false,
        prompt: messages.map(|m| PromptProjection {
            system: None,
            messages: m.into_iter().map(|t| ("user".into(), t.into())).collect(),
        }),
        identity: None,
        signals: Default::default(),
    }
}

/// A transform outcome as comparable JSON.
fn outcome(o: TransformOutcome) -> Value {
    match o {
        TransformOutcome::Rewrite(r) => json!({"rewrite": r.messages, "tools": r.tools}),
        TransformOutcome::Reject { status, message } => {
            json!({"reject": {"status": status, "message": message}})
        }
        TransformOutcome::Abstain => json!("abstain"),
        TransformOutcome::Failed { message } => json!({"failed": message}),
    }
}

/// What one door does with the gate, as one comparable transcript. Metric VALUES that measure wall
/// time (`headroom_overhead_ms_*` sum/min/max) are reduced to their presence; every other byte is
/// compared.
fn transcript(registry: &PluginRegistry, cfg: &str) -> Value {
    let p = registry.resolve("headroom").expect("the alias resolves");
    let stated = Manifest {
        sha256: String::new(),
        signature: String::new(),
        ..p.manifest.clone()
    };
    let policy = registry
        .open_hook("headroom", cfg, "headroom", projectors())
        .expect("the gate opens");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let describe = policy.describe(BUDGET).await;
        let ask = "why did the deployment fail".to_string();
        let history = format!("{}{}", log_dump(40), log_dump(40));
        let scenario = [
            (
                "compressible",
                request("p", Some(vec![history, ask.clone()])),
            ),
            (
                "below-threshold",
                request("p", Some(vec!["hi there".into(), ask.clone()])),
            ),
            ("no-grant", request("p", None)),
            ("malformed", request("malformed", Some(vec![ask.clone()]))),
        ];
        let mut transforms = serde_json::Map::new();
        for (tag, req) in &scenario {
            transforms.insert((*tag).into(), outcome(policy.transform(req, BUDGET).await));
        }
        let mut good = serde_json::Map::new();
        good.insert("target_ratio".into(), json!(0.4));
        let mut bad = serde_json::Map::new();
        bad.insert("bogus_knob".into(), json!(1));
        let ack = policy.configure("headroom", &good, 3, BUDGET).await.is_ok();
        let nack = policy.configure("headroom", &bad, 4, BUDGET).await.is_ok();

        let status = policy.status(BUDGET).await.map(|s| {
            let metrics: Vec<Value> = s
                .metrics
                .unwrap_or_default()
                .into_iter()
                .map(|mut m| {
                    let timed = m["name"].as_str().is_some_and(|n| {
                        n.starts_with("headroom_overhead_ms_") && !n.ends_with("_count")
                    });
                    if timed {
                        m["value"] = json!("<timed>");
                    }
                    m
                })
                .collect();
            json!({"version": s.settings_version, "settings": s.settings, "metrics": metrics})
        });
        json!({
            "row": stated,
            "first_party": p.first_party(),
            "name": policy.name(),
            "describe": describe,
            "configure": [ack, nack],
            "transform": transforms,
            "status": status,
        })
    })
}

/// Headroom registers ONE row and behaves as ONE gate through either door — and the same bytes
/// signed as another kind, or opened differently, do not (the RED arm).
#[test]
fn the_linked_and_the_dropped_in_headroom_gate_are_one_gate() {
    let lib = cdylib();
    let linked = transcript(&linked_registry(), "{}");
    let dropped_in = transcript(&dropped("dropped", statement("hook"), &lib), "{}");
    assert_eq!(linked, dropped_in, "the two doors are not one gate");

    // The scenario did what the gate is for: it compressed the history and kept the ask, abstained
    // below the bar, without a grant and on a malformed projection, ACKed the good push and NACKed
    // the unknown key.
    let t = &linked["transform"];
    let rewritten = t["compressible"]["rewrite"].as_array().expect("a rewrite");
    assert_eq!(rewritten[1]["content"], "why did the deployment fail");
    assert!(rewritten[0]["content"].as_str().unwrap().contains("ERROR"));
    assert_eq!(t["below-threshold"], "abstain");
    assert_eq!(t["no-grant"], "abstain");
    assert_eq!(t["malformed"], "abstain");
    assert_eq!(linked["configure"], json!([true, false]));
    assert_eq!(linked["first_party"], true);
    assert_eq!(linked["status"]["version"], 3);

    // RED (a): the same bytes signed as `store` are refused at the kind handshake, naming both.
    let wrong = dropped("as-store", statement("store"), &lib);
    let e = match wrong.open_store("headroom", "{}") {
        Ok(_) => panic!("a hook library signed as store must be refused"),
        Err(e) => e,
    };
    assert!(
        e.contains("plugin 'headroom' exports kind 'hook' but is being loaded as 'store'"),
        "{e}"
    );

    // RED (b): the same cdylib opened with a savings bar nothing can clear is NOT the same gate.
    let red = transcript(
        &dropped("red", statement("hook"), &lib),
        r#"{"min_savings_pct": 100.0}"#,
    );
    assert_ne!(
        red, linked,
        "a differently configured gate must not compare equal"
    );
}
