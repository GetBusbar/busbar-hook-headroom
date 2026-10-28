// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors

//! THE DOOR: the headroom gate on the hook kind's memory ABI (`hook_door!`, the hook kind's
//! `plugin_door!`), on the SDK's safe surface. No `unsafe` here.
//!
//! * `open` / `refresh` — [`crate::open`]: the operator's settings map to live [`Knobs`], refused
//!   fail-closed (bad JSON, unknown key, out-of-range value).
//! * `transform` — the 1.5.5 projection the SDK rebuilds from the host's view
//!   ([`Decoded::projection_json`]) through the compression pass ([`crate::Headroom::transform`]),
//!   its reply lowered exactly as 1.5.5's host normalized it ([`lower_transform_reply`]).
//! * `decide` / `notify` — the SDK's defaults: ABSTAIN / nothing. Headroom ranks nothing and taps
//!   nothing (1.5.5 answered `{}` and ignored the notify).
//! * `configure` — commit-on-ack, desired state; a NACK is FAILED.
//! * `status` / `describe` — the gate's own documents.
//!
//! [`Knobs`]: crate::compress::Knobs

use std::task::Poll;

use busbar_contract::abi::hook::{CLASS_GATE, PROMPT_RW, Tail, USER_NO};
use busbar_contract::abi::mechanism::door::Statement;
use busbar_contract::abi::sdk::door::statement;
use busbar_contract::abi::sdk::exchange::Op;
use busbar_contract::abi::sdk::hook::{
    Decoded, Hook, HookOpen, RewriteVerdict, lower_transform_reply, statement_with_tail, tail,
};
use busbar_contract::hook_wire::OP_TRANSFORM;

use crate::Headroom;

/// A gate that may see and rewrite the prompt and never reads the caller (1.5.5's manifest
/// `needs: {prompt: rw, user: no}`); the operator's grant decides what it is handed.
const TAIL: &Tail = &tail(CLASS_GATE, PROMPT_RW, USER_NO);

/// How many ops one instance holds in flight; the host clamps.
pub const MAX_INFLIGHT: u32 = 64;

/// THE STATEMENT: name, version and the hook tail. No needs: the gate opens no connection.
pub const STATEMENT: Statement = statement_with_tail(
    statement(crate::NAME, env!("CARGO_PKG_VERSION"), MAX_INFLIGHT),
    TAIL,
);

impl Hook for Headroom {
    fn transform(&self, view: &Decoded<'_>, _op: &Op<'_>) -> Poll<RewriteVerdict> {
        let reply = Headroom::transform(self, &view.projection_json(OP_TRANSFORM));
        Poll::Ready(lower_transform_reply(Ok(reply)))
    }

    fn configure(
        &self,
        settings: &serde_json::Map<String, serde_json::Value>,
        version: u64,
    ) -> bool {
        Headroom::configure(self, settings, version)
    }

    fn status(&self) -> serde_json::Value {
        Headroom::status(self)
    }

    fn describe(&self) -> serde_json::Value {
        Headroom::describe(self)
    }
}

/// Opens the gate: the settings document to its live knobs.
#[derive(Debug)]
pub struct Open;

impl HookOpen for Open {
    fn open(settings: &str) -> Result<Box<dyn Hook>, String> {
        Ok(Box::new(crate::open(settings)?))
    }
}

busbar_contract::hook_door! {
    open: Open,
    statement: STATEMENT,
}
