// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors

//! THE DOOR: the headroom gate on the hook kind's memory ABI (`hook_door!`), on the SDK's safe
//! surface. [`Open`] opens a [`crate::Headroom`] from the operator's settings and bridges it onto the
//! hook kind's typed ops with the SDK's `json_hook`. The Statement carries this plugin's whole
//! `declares` document (BUSBAR-1.6.0.md l.246): `{"hook": {"orders": false}}`, since the gate's
//! `decide` always abstains.
//!
//! The built `cdylib` exports this same door as its one symbol, `busbar_plugin_door`.

use busbar_contract::abi::hook::{CLASS_GATE, PROMPT_RW, Tail, USER_NO};
use busbar_contract::abi::mechanism::door::Statement;
use busbar_contract::abi::sdk::door::statement;
use busbar_contract::abi::sdk::hook::{Hook, HookOpen, json_hook, statement_with_tail, tail};

/// How many ops one instance holds in flight; the host clamps. Compression is CPU-only.
const MAX_INFLIGHT: u32 = 64;

/// The plugin's name: its Statement's, the linked row's and the shipped manifest's
/// (`release.yml` `manifest_name`).
pub(crate) const NAME: &str = "busbar-hook-headroom";

/// A rewrite gate: it reads the prompt at `rw` and asks for no caller identity.
const TAIL: &Tail = &tail(CLASS_GATE, PROMPT_RW, USER_NO);

busbar_contract::statement_extensions!(
    /// The Statement's extensions: this plugin's whole `declares` document (BUSBAR-1.6.0.md l.246).
    EXTENSIONS: [(
        busbar_contract::abi::mechanism::extensions::EXT_DECLARES,
        include_bytes!("../declares.json")
    )]
);

/// THE STATEMENT: name, version, the hook tail and the `declares` document.
const STATEMENT: Statement = {
    let mut s = statement_with_tail(
        statement(NAME, env!("CARGO_PKG_VERSION"), MAX_INFLIGHT),
        TAIL,
    );
    s.extensions = EXTENSIONS;
    s
};

/// Opens a [`crate::Headroom`] from the operator's settings document.
struct Open;

impl HookOpen for Open {
    fn open(settings: &str) -> Result<Box<dyn Hook>, String> {
        crate::open(settings).map(json_hook)
    }
}

busbar_contract::hook_door! {
    open: Open,
    statement: STATEMENT,
}

/// The exported door: this image's ONE symbol, `busbar_plugin_door`, forwards to [`door`]. The
/// macro's `#[unsafe(no_mangle)]` is the one reviewed exemption from `deny(unsafe_code)`.
#[allow(unsafe_code)]
mod exported {
    busbar_contract::export_door!(crate::door);
}
