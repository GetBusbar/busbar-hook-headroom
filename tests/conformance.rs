// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors

//! THE BOTH-WAYS CONFORMANCE of `busbar-hook-headroom`: busbar's PUBLISHED suite
//! (`busbar_test_kit::conformance_suite!`, at the busbar commit this repo pins in `.busbar-ref`),
//! run by this repo. It loads the LINKED door (`door::door`, what a busbar build that compiles this
//! gate in registers) and the BUILT cdylib (this crate's, exporting the same door as
//! `busbar_plugin_door`) through busbar's real plugin loader and drives both with the hook kind's
//! script over `conformance.json`: the two folds must be equal, every step's crossings exactly at
//! the script's pin, and the suite's RED arms refused.
//!
//! The gate declares no need (it opens no connection), so no host connector is named. It never
//! answers an order (`declares.json`: `hook.orders: false`), so the script judges it on
//! abstain-only. The compression pass itself, over a granted prompt, is `e2e.rs`'s.

busbar_test_kit::conformance_suite! {
    door: busbar_hook_headroom::door::door,
    cdylib: "busbar_hook_headroom",
    inputs: include_str!("conformance.json"),
}
