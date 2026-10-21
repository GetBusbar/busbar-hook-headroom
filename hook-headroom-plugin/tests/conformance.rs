// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors

//! THE BOTH-WAYS CONFORMANCE of `busbar-hook-headroom`: busbar's PUBLISHED suite
//! (`busbar_plugin_loader::conformance_suite!`, at the busbar commit this repo pins in `.busbar-ref`),
//! run by this repo. It loads the LINKED door (this crate's `door`, the one a busbar
//! build names) and the BUILT cdylib (this same crate) through busbar's real plugin loader, drives
//! both with the hook kind's script over `conformance.json`, and requires every step's crossings
//! exactly at the script's pin and the two folds equal. The suite's RED arms are kept beside it.
//! `plugin-ci.yml` runs it under `--release`.
//!
//! The compression itself (a granted prompt rewritten, and the same bytes through both doors) is
//! `tests/e2e.rs`: the published script drives no prompt view.

/// This crate's own door, as the loader names a compiled-in one.
#[allow(unused_unsafe)]
fn loaded() -> busbar_plugin_loader::dispatch::LoadedDoor {
    // SAFETY: `door` is this crate's own `hook_door!` entry.
    unsafe { busbar_plugin_loader::dispatch::LoadedDoor::from_raw(busbar_hook_headroom::door) }
}

busbar_plugin_loader::conformance_suite! {
    loaded: loaded(),
    cdylib: "busbar_hook_headroom_plugin",
    inputs: include_str!("conformance.json"),
}
