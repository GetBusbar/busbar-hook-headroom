// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors

//! Explicit creation receipt for each new, exclusively owned memory Store process.

use std::path::Path;
use std::process::Command;

pub fn initialize_and_serve(mut command: Command, directory: &Path, attempt: u32) -> Command {
    // Inspect the actual admitted products/configuration. The inspection opens its own
    // ephemeral Store; its empty state is not evidence that a later Store is fresh.
    let mut inspect = Command::new(command.get_program());
    for (name, value) in command.get_envs() {
        if let Some(value) = value {
            inspect.env(name, value);
        } else {
            inspect.env_remove(name);
        }
    }
    let output = inspect
        .args([
            "--principal-transition",
            "-",
            "--transition-action",
            "inspect",
        ])
        .output()
        .expect("inspect the actual principal requirements");
    assert!(
        output.status.success(),
        "principal inspection failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let inventory: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("principal inspection JSON");
    assert_eq!(inventory["store_ephemeral"], true);
    assert!(inventory["principal_receipt_head_sha256"].is_null());
    assert!(
        inventory["requirements"]
            .as_array()
            .is_some_and(|requirements| !requirements.is_empty())
    );

    // This harness owns the uniquely created scratch directory and starts a new memory
    // Store in each child. No retained backend or shared sender exists. Bind that
    // provenance to this attempt and retain it beside the receipt, rather than using a
    // fabricated digest or treating Store ephemerality alone as creation evidence.
    let evidence = serde_json::to_vec(&serde_json::json!({
        "fixture_directory": directory,
        "owner_process": std::process::id(),
        "attempt": attempt,
        "binary": command.get_program(),
        "inspection_sha256": busbar_contract::redacted::sha256_hex(&output.stdout),
        "creation": "new child-owned memory Store; fixture directory created exclusively",
        "ownership": "only this test starts or accesses this in-process namespace",
        "sender_quiesced": true,
        "backend_quiesced": true,
    }))
    .unwrap();
    let evidence_path = directory.join(format!("principal-creation-{attempt}.json"));
    write_new(&evidence_path, &evidence);
    let receipt = serde_json::json!({
        "version": 1,
        "store_config_sha256": inventory["store_config_sha256"],
        "requirements": inventory["requirements"],
        "witness": null,
        "ephemeral_instance": true,
        "exclusive_instance": true,
        "creation_evidence_sha256": busbar_contract::redacted::sha256_hex(&evidence),
        "newly_created_namespace": true,
        "sender_quiesced": true,
        "backend_quiesced": true,
        "actor": "Headroom e2e fixture owner",
        "reason": "new exclusive child-owned memory Store; no retained state or application senders",
    });
    let receipt_path = directory.join(format!("principal-fresh-{attempt}.json"));
    write_new(&receipt_path, &serde_json::to_vec(&receipt).unwrap());
    // Initialize and serve on the SAME open; a separate cold initializer cannot carry
    // an in-memory baseline into the serving child. All ordinary boot guards remain.
    command
        .arg("--principal-transition")
        .arg(receipt_path)
        .args(["--transition-action", "initialize-fresh-and-serve"]);
    command
}

fn write_new(path: &Path, bytes: &[u8]) {
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .expect("exclusively create per-attempt namespace evidence")
        .write_all(bytes)
        .expect("write namespace evidence");
}
