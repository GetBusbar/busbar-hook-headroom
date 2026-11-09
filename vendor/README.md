# Pinned dependency corrections

These Cargo path patches keep Headroom on its existing Busbar SDK revision while removing the
unmaintained `unsafe-libyaml` and `paste` packages. They are third-party source snapshots with the
bounded changes below, not new upstream releases. `Cargo.lock` selects `macro_rules_attribute`
0.2.3 (and its matching proc-macro crate), which uses `pastey` for the indirect macro dependency.
The two retained `pastey` versions follow the published upstream requirements.

## YAML

- Package: `serde_yaml_ng` 0.10.0, MIT; retain `LICENSE` and `CONTRIBUTORS`.
- Existing upstream: <https://github.com/acatton/serde-yaml-ng/pull/29>.
- Exact source commit: `4d41b1443db4f42a00c059109d40f706d2f87489`.
- SHA-256 of `git archive` of that commit (uncompressed tar):
  `bed183bdd9b8c50e6fc7417f07b81751fdd8dcf44b5ee98205a15493c0c62c07`.
- Retained payload: `Cargo.toml`, `LICENSE`, `CONTRIBUTORS`, `README.md`, and `src/`.
  Upstream CI, fuzz harnesses, and upstream tests are omitted; no production source is omitted.
- Local delta: `serde-yaml-ng-local.patch`, relative to the exact upstream commit.
  `src/libyaml/emitter.rs` owns the writer allocation through an optional pointer and destroys the
  borrowing backend before transferring or dropping that allocation. Normal drop, write errors,
  serialization errors, and unwinding release the writer exactly once; `into_inner` transfers it.
  `src/libyaml/util.rs` makes the slice-storage lifetime/exclusivity obligation an explicit unsafe
  constructor contract. `src/libyaml/parser.rs` documents the allocation-retention obligation at its two
  call sites, retaining allocations until after the borrowing parser is dropped.
- Backend: registry `libyaml-safer` 0.1.1, MIT (`LICENSE-MIT`), pinned by the lockfile.

This is an unmerged backend migration plus a local ownership correction. Its unchanged package
name/version lets the SDK and regression tests share one Rust `Value` type. This does not establish
full YAML equivalence or memory-safety certification. The ordinary native qualification remains
required. The local integration tests cover writer custody and the SDK's exercised YAML behavior.

## Tokenizers

- Package: `tokenizers` 0.23.2, Apache-2.0; retain `LICENSE`.
- Published archive: <https://crates.io/api/v1/crates/tokenizers/0.23.2/download>.
- Archive SHA-256: `7afbf6e88718afcc138bad01d6ccc3051dbbc3b2ce9793d8b8a3aeb610969cfc`.
- Retained payload: `Cargo.toml`, `Cargo.toml.orig`, `LICENSE`, `README.md`, and `src/`.
  Release notes, package lockfile, upstream tests, benchmark fixtures, and maintenance scripts are
  omitted; no production source is omitted. The unchanged upstream manifest's explicit test/bench
  paths are not part of Headroom's dependency build; run Headroom's own qualification commands.
- Local delta: `tokenizers-local.patch`. Backport the official two-call-site `pastey` replacement
  from <https://github.com/huggingface/tokenizers/commit/b43d8d734de6481d8a266e83ddeb90103f1ec355>:
  change both `paste::paste!` calls in `src/utils/mod.rs` to `pastey::paste!`, and update the dependency
  in both package manifests from `paste` 1.0.14 to `pastey` 0.1.0 (lock resolves 0.1.1).
  `pastey` is MIT OR Apache-2.0; `macro_rules_attribute` is Apache-2.0 OR MIT OR Zlib.

## Reproduction and maintenance

Fetch the exact YAML Git commit and the checksum-verified tokenizers registry archive, retain the
payload paths above, then apply the corresponding patch with `patch -p1` inside each package.
`SHA256SUMS` records every retained source/package/license/readme file after the corrections.
The SDK source and SDK pin are unchanged. The patches are local paths; no new Git source admission,
advisory ignore, or policy exemption is required.

Maintain these snapshots explicitly: compare each future upstream release, preserve notices, replay
only still-needed corrections, update source/archive hashes and lockfile together, and rerun the
writer, YAML/SDK, compression, architecture, and dependency-policy checks. Remove the local snapshots
when compatible upstream releases contain the needed changes and pass qualification. Vendoring
retains the full production source footprint; it is not a claim that this code is maintained by
Headroom's upstreams or excluded from production source accounting.
