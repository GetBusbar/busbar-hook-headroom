// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors

//! End-to-end coverage of the `headroom` compression gate over busbar's REAL loader, through BOTH
//! doors: the LINKED door (`door`, what a busbar build that compiles this crate in
//! registers) and the DROPPED-IN door (this crate's built cdylib). Each is admitted through the one
//! loader, opened over settings, and driven with `transform` over the host's view of a granted
//! prompt, whose rewrite compresses with the REAL `headroom-core` BM25 `TextCrusher` engine.
//!
//! The corpus fixtures (`log_dump`) are shared in spirit with `src/tests/compress.rs`: a toy 2-word
//! string never triggers BM25/TextCrusher's real compression path, since `TextCrusher` passes short
//! texts (<6 segments) through unchanged.

use std::sync::Arc;

use busbar_contract::abi::hook::{DecideOut, TransformOut, VERB_ABSTAIN, VERB_REWRITE, slot};
use busbar_contract::abi::host::hook::{DecideFrame, DecideView};
use busbar_contract::abi::mechanism::call::{BLOB_JSON, Blob, Outcome};
use busbar_contract::abi::mechanism::lifecycle::{OpenIn, OpenOut, slot as life};
use busbar_contract::abi::sdk::door::blank_out;
use busbar_contract::hooks::{Candidate, PromptProjection, RoutingContext, RoutingRequest};
use busbar_plugin_loader::dispatch::kinds::hook::Hook;
use busbar_plugin_loader::dispatch::{
    Bind, ConnTable, DispatchConfig, Dispatcher, Frame, LinkedRow, LoadedDoor, NoSink, Plugin,
    in_head, load_dropped, load_linked, out_head, rendering_of_loaded,
};

fn bind(d: &Dispatcher) -> Bind {
    Bind {
        instance: Arc::from("headroom"),
        max_inflight_cap: 64,
        sink: Arc::new(NoSink),
        dispatcher: d.adopter(),
        conns: ConnTable::NoNeeds,
    }
}

/// This crate's own `door`, as the loader names a compiled-in one.
#[allow(unused_unsafe)]
fn loaded() -> LoadedDoor {
    // SAFETY: `door` is this crate's own `hook_door!` entry.
    unsafe { LoadedDoor::from_raw(busbar_hook_headroom::door) }
}

/// The linked door, bound on `d`.
fn linked(d: &Dispatcher) -> Plugin<Hook> {
    let row = LinkedRow::of_loaded(loaded()).expect("the door states itself");
    load_linked::<Hook>(&row, bind(d)).expect("the door loads")
}

/// This crate's dropped-in image, the cdylib `cargo test` builds, admitted against the Statement the
/// linked door renders. A missing artifact is a failure, never a skip: this test IS the dropped-in
/// door's proof.
fn dropped(d: &Dispatcher) -> Plugin<Hook> {
    let path = busbar_plugin_loader::conformance::cdylib_of("busbar_hook_headroom");
    let stated = rendering_of_loaded(loaded()).expect("the door renders its Statement");
    load_dropped::<Hook>(&path, &stated, bind(d)).expect("the dropped door loads")
}

/// `open` over `settings`: its outcome.
fn open(p: &Plugin<Hook>, settings: &str) -> Outcome {
    let mut f = Frame::new(
        OpenIn {
            head: in_head(),
            host: std::ptr::null(),
            // SAFETY: `settings` outlives the call, which is the only reader of the blob.
            settings: unsafe {
                Blob::from_raw_parts(settings.as_ptr(), settings.len(), BLOB_JSON, 0)
            },
            secrets: std::ptr::null(),
            secrets_len: 0,
            generation: 1,
            err_buf: std::ptr::null_mut(),
            err_cap: 0,
        },
        OpenOut {
            head: out_head(),
            instance: std::ptr::null_mut(),
            err_len: 0,
        },
    );
    p.call(life::OPEN, &mut f).outcome
}

/// A synthetic log-dump line set: many segments, mostly noise, one load-bearing ERROR, realistic
/// enough for BM25 relevance scoring to actually distinguish signal from noise.
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

/// A request carrying `messages` as the granted prompt (`None` = grant absent).
fn request(id: u64, messages: Option<Vec<String>>) -> RoutingRequest<'static> {
    let total_chars = messages
        .as_ref()
        .map_or(0, |m| m.iter().map(String::len).sum());
    RoutingRequest {
        request_id: id,
        pool: "p",
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
        session: None,
    }
}

fn history() -> String {
    format!("{}{}", log_dump(40), log_dump(40))
}

const ASK: &str = "why did the deployment fail";

fn ctx() -> RoutingContext<'static> {
    RoutingContext {
        pool: "p",
        budget_remaining: None,
        budget: &[],
    }
}

/// One `transform` over `req`: its outcome, verbs and the rewrite bytes it wrote. A short answer is
/// re-called ONCE over the buffers it named, as the host does.
fn transform(p: &Plugin<Hook>, req: &RoutingRequest<'_>) -> (Outcome, u32, Vec<u8>) {
    let mut frame = DecideFrame::first(DecideView::build(req, &[], &ctx()));
    for _ in 0..2 {
        let mut f = Frame::new(frame.input(), blank_out::<TransformOut>());
        let outcome = p.call(slot::TRANSFORM, &mut f).outcome;
        if outcome == Outcome::Failed {
            frame = frame.regrown_transform(&f.out);
            continue;
        }
        return (outcome, f.out.verbs, frame.rewrite(f.out.rewrite_written));
    }
    panic!("a transform that stays short after its one re-call");
}

/// One `decide` over a single candidate: its outcome and verbs.
fn decide(p: &Plugin<Hook>, req: &RoutingRequest<'_>) -> (Outcome, u32) {
    let cand = Candidate {
        idx: 0,
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
        signals: Default::default(),
    };
    let frame = DecideFrame::first(DecideView::build(req, &[cand], &ctx()));
    let mut f = Frame::new(frame.input(), blank_out::<DecideOut>());
    let outcome = p.call(slot::DECIDE, &mut f).outcome;
    (outcome, f.out.verbs)
}

/// Both doors, each on a dispatcher of its own, opened over `settings`.
fn both(settings: &str, check: impl Fn(&str, &Plugin<Hook>)) {
    for (door, load) in [
        ("linked", linked as fn(&Dispatcher) -> Plugin<Hook>),
        ("dropped in", dropped),
    ] {
        let d = Dispatcher::new(DispatchConfig::default());
        let p = load(&d);
        assert_eq!(open(&p, settings), Outcome::Ready, "{door}: open");
        check(door, &p);
    }
}

/// END-TO-END: `transform` of a realistic compressible history (grant given) is a rewrite carrying
/// the shrunk message bodies in BODY form (`{"role","content"}`: the deliberate asymmetry from the
/// old socket hook, input is read from `text`, output is emitted as `content`).
#[test]
fn transform_compresses_granted_history() {
    both("{}", |door, p| {
        let history = history();
        let req = request(1001, Some(vec![history.clone(), ASK.to_string()]));
        let (outcome, verbs, bytes) = transform(p, &req);
        assert_eq!(outcome, Outcome::Ready, "{door}");
        assert_eq!(verbs, VERB_REWRITE, "{door}");
        let rw: serde_json::Value = serde_json::from_slice(&bytes).expect("the rewrite is JSON");
        let messages = rw["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 2, "{door}");
        assert_eq!(messages[1]["content"], ASK, "{door}");
        let first = messages[0]["content"].as_str().expect("content");
        assert!(first.contains("ERROR"), "{door}");
        assert!(
            messages[0].get("text").is_none(),
            "{door}: reply must be BODY form (content), not projection form (text)"
        );
        assert!(
            first.len() < history.len(),
            "{door}: the history must shrink"
        );
    });
}

/// `transform` of an already-tight prompt abstains (nothing to compress; the original body proceeds).
#[test]
fn transform_abstains_when_nothing_to_compress() {
    both("{}", |door, p| {
        let req = request(
            1003,
            Some(vec!["hi".to_string(), "how are you".to_string()]),
        );
        let (outcome, verbs, _) = transform(p, &req);
        assert_eq!((outcome, verbs), (Outcome::Ready, VERB_ABSTAIN), "{door}");
    });
}

/// `transform` with NO prompt projected (operator grant absent) abstains. The gate can never coerce
/// content: without the projection it simply has nothing to rewrite.
#[test]
fn transform_abstains_without_grant() {
    both("{}", |door, p| {
        let (outcome, verbs, _) = transform(p, &request(1002, None));
        assert_eq!((outcome, verbs), (Outcome::Ready, VERB_ABSTAIN), "{door}");
    });
}

/// Abstain when `min_savings_pct` is 100: even a real, shrinking history abstains once the savings
/// bar is unreachable. This is also the RED arm of the both-ways comparison: the same history over
/// the same door, opened differently, is not the same gate.
#[test]
fn transform_abstains_when_min_savings_unreachable() {
    both(r#"{"min_savings_pct": 100.0}"#, |door, p| {
        let req = request(1004, Some(vec![history(), ASK.to_string()]));
        let (outcome, verbs, _) = transform(p, &req);
        assert_eq!((outcome, verbs), (Outcome::Ready, VERB_ABSTAIN), "{door}");
    });
}

/// `decide` abstains over the door (the gate ranks nothing).
#[test]
fn decide_abstains() {
    both("{}", |door, p| {
        let req = request(1005, Some(vec!["hi".to_string(), "x".to_string()]));
        assert_eq!(decide(p, &req), (Outcome::Ready, VERB_ABSTAIN), "{door}");
    });
}

/// A malformed settings document, an unknown key and an out-of-range value each fail the open
/// through both doors (fail closed: better a loud load failure than a silently mis-parsed knob).
#[test]
fn open_refuses_bad_settings() {
    for door in [linked as fn(&Dispatcher) -> Plugin<Hook>, dropped] {
        let d = Dispatcher::new(DispatchConfig::default());
        for bad in [
            "{ not json",
            r#"{"bogus_knob": 1}"#,
            r#"{"target_ratio": 2.0}"#,
        ] {
            assert_ne!(open(&door(&d), bad), Outcome::Ready, "{bad}");
        }
    }
}

/// The linked row states the name the shipped tarball's manifest does (`release.yml`
/// `manifest_name`), so config `module: busbar-hook-headroom` resolves on a build that links this
/// crate.
#[test]
fn linked_row_states_the_shipped_manifest_name() {
    let stated = rendering_of_loaded(loaded()).expect("the door renders its Statement");
    let name = b"busbar-hook-headroom".as_slice();
    assert!(
        stated.windows(name.len()).any(|w| w == name),
        "the Statement's rendering names the plugin"
    );
}

/// The door's Statement carries this plugin's whole `declares` document (BUSBAR-1.6.0.md l.246):
/// the gate never orders, and says so.
#[test]
fn the_statement_carries_the_declares_document() {
    let row = LinkedRow::of_loaded(loaded()).unwrap();
    let read = busbar_contract::abi::mechanism::rendering::read(&row.statement).unwrap();
    let declares = busbar_contract::abi::mechanism::extensions::declares(&read.extensions.bytes)
        .expect("the Statement carries the declares document");
    assert_eq!(declares, &include_bytes!("../declares.json")[..]);
    let doc: serde_json::Value = serde_json::from_slice(declares).unwrap();
    assert_eq!(doc["hook"]["orders"], false);
}
