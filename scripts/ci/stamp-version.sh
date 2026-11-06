#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Stamp <version> into the root Cargo.toml [package] `version` and re-resolve Cargo.lock, so the
# commit release-on-upstream tags v<version> declares the version it is released as. Keeps
# next-version.sh (the greater of the tag bump and Cargo.toml) and headroom-release-publish.yml
# (which tags the Cargo.toml version) in agreement with every tag this repo has.
#
# Usage: stamp-version.sh <X.Y.Z>     (run from the repo root; needs cargo)
set -euo pipefail
ver="${1:?usage: stamp-version.sh <X.Y.Z>}"; ver="${ver#v}"
[[ "$ver" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "::error::stamp: '$ver' is not X.Y.Z" >&2; exit 1; }
[ -f Cargo.toml ] && [ -f Cargo.lock ] || { echo "::error::stamp: run from the repo root" >&2; exit 1; }
awk -v v="$ver" '
  /^\[package\]/ {p=1; print; next}
  /^\[/ {p=0}
  p && !done && /^version *= *"/ {print "version = \"" v "\""; done=1; next}
  {print}
  END {if (!done) exit 3}
' Cargo.toml > Cargo.toml.stamp || { rm -f Cargo.toml.stamp; echo "::error::stamp: no [package] version in Cargo.toml" >&2; exit 1; }
mv Cargo.toml.stamp Cargo.toml
cargo metadata --format-version 1 >/dev/null
cargo metadata --format-version 1 --locked >/dev/null
echo "Cargo.toml [package] version -> ${ver}"
