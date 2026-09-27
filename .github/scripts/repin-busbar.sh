#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Move this repo's busbar pin to <sha> (busbar version <version>) in ONE step, so the three places
# that name it can never disagree:
#
#   * `.busbar-ref`        — "<sha> <version>", the record release.yml and release-on-upstream read;
#   * every Cargo.toml     — each `busbar-* = { git = "https://github.com/GetBusbar/busbar", rev = … }`
#                            (busbar-contract, and busbar-plugin-loader for the tests), which is what
#                            cargo actually BUILDS against;
#   * Cargo.lock           — the resolved `git+https://github.com/GetBusbar/busbar?rev=<sha>#<sha>`
#                            source, so `--locked` builds keep working.
#
# Rewriting `.busbar-ref` alone (the 1.5.x shape, when busbar was a sibling path checkout) would leave
# the manifests building the OLD contract while the record claimed the new one — ci.yml's `pin` job
# refuses exactly that, so a release cut that way could never pass its own gate.
#
# Usage: repin-busbar.sh <40-hex sha> <version>     (run from the repo root; needs cargo + network)
#        repin-busbar.sh --decide <sha|""> <version|"">
#
# --decide answers release-on-upstream's question "did upstream move?" against `.busbar-ref`, and
# prints `cut` or `skip` on stdout (the reason on stderr). Upstream moved when the incoming busbar
# RELEASE COMMIT is not the recorded one — by version when it is newer, and by commit when the
# version string is the same: a v1.6.0 tag whose commit differs from the recorded sha (the pin was
# taken from a pre-release commit that later carried the same version number) IS a new upstream
# and must cut. Same version + same sha, an older version, or no incoming version skips. Needs
# neither cargo nor network.
set -euo pipefail

if [ "${1:-}" = "--decide" ]; then
  in_sha="${2-}"; in_ver="${3-}"; in_ver="${in_ver#v}"
  rec_sha=""; rec_ver=""
  if [ -f .busbar-ref ]; then
    rec_sha="$(cut -d' ' -f1 .busbar-ref)"
    rec_ver="$(cut -d' ' -f2 .busbar-ref | tr -d '[:space:]')"
  fi
  if [ -n "$in_sha" ] && ! [[ "$in_sha" =~ ^[0-9a-f]{40}$ ]]; then
    echo "::error::decide: incoming sha '$in_sha' is not a full 40-hex busbar sha" >&2; exit 1
  fi
  say() { echo "::notice::$2" >&2; echo "$1"; }
  if [ -z "$in_ver" ]; then
    say skip "no incoming busbar version (recorded=${rec_ver:-none}) -> nothing to do"
  elif [ -z "$rec_ver" ]; then
    say cut "no recorded busbar version -> cutting against busbar ${in_ver}"
  elif [ "$in_ver" = "$rec_ver" ]; then
    if [ -z "$in_sha" ]; then
      say skip "incoming busbar ${in_ver} == recorded, and no incoming commit to compare -> nothing to do"
    elif [ "$in_sha" = "$rec_sha" ]; then
      say skip "incoming busbar ${in_ver} at ${in_sha} == recorded pin -> nothing to do"
    else
      say cut "upstream moved: busbar ${in_ver} resolves to ${in_sha}, recorded pin is ${rec_sha:-none} -> cutting a release"
    fi
  elif [ "$(printf '%s\n%s\n' "$rec_ver" "$in_ver" | sort -V | tail -1)" = "$in_ver" ]; then
    say cut "busbar advanced (recorded=${rec_ver} -> ${in_ver}) -> cutting a release"
  else
    say skip "incoming busbar ${in_ver} not newer than recorded ${rec_ver} -> nothing to do"
  fi
  exit 0
fi

sha="${1:?usage: repin-busbar.sh <sha> <version>}"
ver="${2:?usage: repin-busbar.sh <sha> <version>}"
ver="${ver#v}"
src='https://github.com/GetBusbar/busbar'

[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || { echo "::error::repin: '$sha' is not a full 40-hex busbar sha" >&2; exit 1; }
[[ "$ver" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-.+][0-9A-Za-z.-]+)?$ ]] || { echo "::error::repin: '$ver' is not a version" >&2; exit 1; }
[ -f Cargo.lock ] || { echo "::error::repin: run from the repo root (no Cargo.lock here)" >&2; exit 1; }

manifests=()
while IFS= read -r m; do manifests+=("$m"); done < <(git ls-files '*Cargo.toml' | xargs grep -l "git = \"${src}\"" || true)
[ "${#manifests[@]}" -gt 0 ] || { echo "::error::repin: no manifest names the busbar git source" >&2; exit 1; }

for m in "${manifests[@]}"; do
  sed -i.repin-bak -E "s#(git = \"${src}\", rev = \")[0-9a-f]+\"#\\1${sha}\"#g" "$m"
  rm -f "$m.repin-bak"
done
printf '%s %s\n' "$sha" "$ver" > .busbar-ref

# Re-resolve ONLY what moved (the busbar git source); every other locked package stays as locked.
cargo metadata --format-version 1 >/dev/null
cargo metadata --format-version 1 --locked >/dev/null

found="$(grep -hoE "git = \"${src}\", rev = \"[0-9a-f]+\"" "${manifests[@]}" | grep -oE '[0-9a-f]{40}' | sort -u)"
[ "$found" = "$sha" ] || { echo "::error::repin: manifests name ${found//$'\n'/ } after the rewrite, not $sha" >&2; exit 1; }
if grep -E "^source = \"git\+${src}\?" Cargo.lock | grep -vq "rev=${sha}#${sha}\""; then
  echo "::error::repin: Cargo.lock still names a busbar source other than ${sha}" >&2; exit 1
fi
grep -q "rev=${sha}#${sha}\"" Cargo.lock || { echo "::error::repin: Cargo.lock has no busbar source at ${sha}" >&2; exit 1; }

echo "busbar pin -> ${sha} ${ver} (${#manifests[@]} manifest(s), .busbar-ref, Cargo.lock)"
