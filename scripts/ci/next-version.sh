#!/usr/bin/env bash
# next-version.sh — compute THIS repo's next release tag for release-on-upstream.yml.
#
# Single source of truth for the version math, exercised in CI by release-selftest.yml so the
# release automation can't silently rot. Prints "v<MAJOR>.<MINOR>.<PATCH>" to stdout.
#
# Inputs (env, all optional):
#   INPUT_VERSION    explicit version to cut (leading "v" tolerated) -> used verbatim.
#   INITIAL_VERSION  first-release default when the repo has NO prior v* tag (default "1.0.0").
#
# Behaviour:
#   * explicit INPUT_VERSION            -> v<INPUT_VERSION>
#   * no prior v* tag (brand-new repo)  -> v<INITIAL_VERSION>   (NEVER errors — first release)
#   * otherwise                         -> the GREATER of
#         - the patch-bump of the highest existing v* tag, and
#         - the root Cargo.toml [package] version (when a Cargo.toml is present)
#     so the result is always above every existing tag AND never below the version the crate
#     declares. headroom-release-watch.yml bumps Cargo.toml on dev for every headroom-core update,
#     and headroom-release-publish.yml tags that Cargo.toml version; computing from the tags alone
#     (the old rule) gave release-on-upstream v2.0.8 while Cargo.toml already said 2.0.22 — two
#     release paths disagreeing about the next version. release-on-upstream stamps the tag it cuts
#     back into Cargo.toml, so after any cut the two agree again.
# Fully `set -u` safe: every variable is initialised before use, so no "unbound variable".
set -euo pipefail

input="${INPUT_VERSION:-}"
initial="${INITIAL_VERSION:-1.0.0}"

if [ -n "$input" ]; then
  printf 'v%s\n' "${input#v}"
  exit 0
fi

latest="$(git tag --list 'v*' | sort -V | tail -1 || true)"
if [ -z "${latest:-}" ]; then
  # FIRST RELEASE: no prior v* tag. Default to the declared initial version instead of
  # erroring, so a brand-new plugin is cuttable by the release train.
  printf 'v%s\n' "${initial#v}"
  exit 0
fi

base="${latest#v}"
major="${base%%.*}";  rest="${base#*.}"
minor="${rest%%.*}";  patch="${rest#*.}"
patch="${patch%%[-+]*}"          # drop any -rc / +build suffix on the patch component
# Coerce every component to a non-negative integer so arithmetic under set -u never explodes.
case "$major" in ''|*[!0-9]*) major=0 ;; esac
case "$minor" in ''|*[!0-9]*) minor=0 ;; esac
case "$patch" in ''|*[!0-9]*) patch=0 ;; esac
next="${major}.${minor}.$((patch + 1))"

# The crate's declared version: the first `version = "X.Y.Z"` of the root Cargo.toml's [package].
declared=""
if [ -f Cargo.toml ]; then
  declared="$(awk '/^\[package\]/{p=1; next} /^\[/{p=0} p && /^version *= *"/{gsub(/.*= *"|".*/, ""); print; exit}' Cargo.toml)"
fi
if [[ "$declared" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
   && [ "$(printf '%s\n%s\n' "$next" "$declared" | sort -V | tail -1)" = "$declared" ]; then
  next="$declared"
fi
printf 'v%s\n' "$next"
