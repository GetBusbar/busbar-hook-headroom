#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stamp <version> into both crates' Cargo.toml [package] `version` (hook-headroom, hook-headroom-plugin) and re-resolve Cargo.lock, so the
# commit release-on-upstream tags v<version> declares the version it is released as. Keeps
# next-version.sh (the greater of the tag bump and Cargo.toml) and headroom-release-publish.yml
# (which tags the Cargo.toml version) in agreement with every tag this repo has.
#
# Usage: stamp-version.sh <X.Y.Z>     (run from the repo root; needs cargo)
set -euo pipefail
ver="${1:?usage: stamp-version.sh <X.Y.Z>}"; ver="${ver#v}"
[[ "$ver" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "::error::stamp: '$ver' is not X.Y.Z" >&2; exit 1; }
[ -f hook-headroom/Cargo.toml ] && [ -f hook-headroom-plugin/Cargo.toml ] && [ -f Cargo.lock ] || { echo "::error::stamp: run from the repo root" >&2; exit 1; }
for f in hook-headroom/Cargo.toml hook-headroom-plugin/Cargo.toml; do
  awk -v v="$ver" '
    /^\[package\]/ {p=1; print; next}
    /^\[/ {p=0}
    p && !done && /^version *= *"/ {print "version = \"" v "\""; done=1; next}
    {print}
    END {if (!done) exit 3}
  ' "$f" > "$f.stamp" || { rm -f "$f.stamp"; echo "::error::stamp: no [package] version in $f" >&2; exit 1; }
  mv "$f.stamp" "$f"
done
cargo metadata --format-version 1 >/dev/null
cargo metadata --format-version 1 --locked >/dev/null
echo "crate [package] versions -> ${ver}"
