#!/usr/bin/env python3
# Copyright (C) 2026 Busbar Inc and contributors
#
# Docker A/B benchmark for the Headroom hook — the same methodology as busbar's own *_ab.py
# harnesses, driven against an explicitly selected compatible busbar image the way a real install
# runs it. It measures the hook's added cost on busbar's OWN clock (`busbar;dur`), so the
# harness/network floor cancels in the with/without-hook delta.
#
# Topology (busbar's signed dlopen plugin ABI, v2 at the busbar rev in .busbar-ref):
#   * mock upstream — a recording OpenAI-shaped mock on 127.0.0.1:9001; tallies the chars that
#     actually ARRIVED upstream, proving the rewrite shipped rather than being hook-side accounting.
#   * busbar — ONE container, the dynamically linked busbar image named by BUSBAR_IMAGE (the
#     static, plugin-free getbusbar/busbar image cannot dlopen the hook), sharing the mock container's NETWORK
#     NAMESPACE so the mock is reachable at 127.0.0.1 (busbar's plaintext-loopback carve-out, same
#     as a local-model deployment). Its :8080 is published through the mock container.
#
# A `kind: hook` plugin loads IN-PROCESS (dlopen) inside the one busbar container. The hook phase
# differs from the baseline phase ONLY in which config.yaml is mounted (config.hook.yaml adds
# `plugins:`, the `hooks:` definition and the `pools.hooks` attach) and in a `plugins/` directory
# bind-mounted at /etc/busbar/plugins containing the Headroom plugin tarball — see `prep_plugin()`.
#
# The plugin tarball is built and packed from THIS checkout (not pulled from a release), so the
# bench always measures the current source tree: `cargo build --release` here, then
# `busbar-plugin-pack pack --allow-unsigned` (the busbar-plugin-pack binary is built from the
# busbar checkout — see BUSBAR_DIR below). Both busbar and the plugin must target the SAME OS/
# arch as the container (Linux); on a Linux host of that arch a plain `cargo build --release` is
# correct. A non-Linux dev host (e.g. macOS) cannot run this harness: a locally-built .dylib
# cannot dlopen inside a Linux container.
#
#   BUSBAR_IMAGE=<compatible-image> python3 docker_ab.py --requests 1000 --concurrency 1 --history-kb 11
#
# Stdlib only (+ the docker CLI + cargo). Writes results/docker_ab.json and prints a summary.

import argparse
import http.client
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)  # headroom-hook checkout root
sys.path.insert(0, HERE)  # corpus.py is a sibling in this folder
import corpus  # noqa: E402

BUSBAR_IMAGE = os.environ.get("BUSBAR_IMAGE")
MOCK = "hb-mock"
BUSBAR = "hb-busbar"
# INTERNAL ports (inside the shared netns — fixed, never collide): busbar listens on
# 8080 (see config*.yaml), the mock on 9001 (providers.mock.yaml points busbar there).
PORT = 8080
MOCK_PORT = 9001
# The only HOST-published port; override on a busy host with BENCH_PORT. The driver
# and healthz poll this; the mock's /stats,/reset are reached in-container via docker exec.
HOST_PORT = int(os.environ.get("BENCH_PORT", "8080"))
MOCK_READY_TRIES = 30  # x 0.5 s
PLUGIN_DIR = os.path.join(HERE, "plugins")  # bind-mounted read-only at /etc/busbar/plugins


def sh(*args, check=True, quiet=True, cwd=None):
    """Run a command. A quiet one hides its output unless it fails, then shows its stderr."""
    if not quiet:
        return subprocess.run(args, check=check, cwd=cwd)
    done = subprocess.run(args, cwd=cwd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    if check and done.returncode != 0:
        sys.stderr.write(done.stderr)
        done.check_returncode()
    return done


def rm(*names):
    for n in names:
        sh("docker", "rm", "-f", n, check=False)


def percentile(vals, q):
    if not vals:
        return None
    s = sorted(vals)
    i = min(len(s) - 1, max(0, round(q * (len(s) - 1))))
    return s[i]


def busbar_dur_us(server_timing):
    # Server-Timing: busbar;dur=<milliseconds> -> integer microseconds.
    for part in (server_timing or "").split(","):
        part = part.strip()
        if part.startswith("busbar;dur="):
            try:
                return round(float(part.split("=", 1)[1]) * 1000.0)
            except ValueError:
                return None
    return None


def wait_ready(timeout=25.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(f"http://localhost:{HOST_PORT}/healthz", timeout=1) as r:
                if r.status == 200:
                    return True
        except Exception:
            time.sleep(0.3)
    return False


def openai_body(history_kb):
    # The deterministic tool-log history as an OpenAI /v1/chat/completions body.
    msgs = corpus.history_messages("tool_log", history_kb * 1024)
    return json.dumps({"model": "bench-pool", "messages": msgs}).encode()


def mock_get(path):
    # The mock binds 127.0.0.1 inside its own netns, so reach its control endpoints
    # (/stats, /reset) from INSIDE that container (it ships python).
    method = "POST" if path == "/reset" else "GET"
    code = (
        "import urllib.request as u,sys;"
        f"r=u.urlopen(u.Request('http://127.0.0.1:{MOCK_PORT}{path}',method='{method}'));"
        "sys.stdout.write(r.read().decode())"
    )
    done = subprocess.run(
        ["docker", "exec", MOCK, "python", "-c", code],
        capture_output=True, text=True,
    )
    if done.returncode != 0:
        raise RuntimeError(f"mock upstream {path} failed: {done.stderr.strip()}")
    return json.loads(done.stdout) if done.stdout.strip() else {}


def load(body, n, conc, warmup):
    """Drive n keep-alive POSTs (after `warmup`), collect busbar;dur µs samples."""
    if n <= 0 or conc <= 0 or warmup < 0:
        raise ValueError("requests and concurrency must be positive; warmup must be nonnegative")
    hdrs = {"content-type": "application/json"}

    def run(count):
        durs = []
        conn = http.client.HTTPConnection("localhost", HOST_PORT, timeout=30)
        try:
            for _ in range(count):
                conn.request("POST", "/v1/chat/completions", body=body, headers=hdrs)
                r = conn.getresponse()
                st = r.getheader("Server-Timing")
                payload = r.read()
                if not 200 <= r.status < 300:
                    detail = payload[:500].decode("utf-8", "replace")
                    raise RuntimeError(f"benchmark request returned HTTP {r.status}: {detail}")
                d = busbar_dur_us(st)
                if d is None:
                    raise RuntimeError("benchmark response has no valid busbar Server-Timing")
                durs.append(d)
        finally:
            conn.close()
        return durs

    run(warmup)
    if conc == 1:
        return run(n)
    from concurrent.futures import ThreadPoolExecutor

    workers = min(conc, n)
    per, remainder = divmod(n, workers)
    with ThreadPoolExecutor(max_workers=workers) as executor:
        futures = [executor.submit(run, per + (i < remainder)) for i in range(workers)]
        return [duration for future in futures for duration in future.result()]


def start_mock(delay_ms):
    rm(MOCK)
    # Publish only busbar's data port (busbar shares this netns and listens on PORT
    # internally). The mock's own port stays internal — reached via docker exec.
    sh("docker", "run", "-d", "--name", MOCK, "-p", f"{HOST_PORT}:{PORT}",
       "-v", f"{HERE}/mock_upstream.py:/mock.py:ro",
       "python:3.12-slim", "python", "/mock.py", "--port", str(MOCK_PORT),
       "--delay-ms", str(delay_ms))
    # Poll the mock until it answers rather than assuming a fixed start-up time.
    for _ in range(MOCK_READY_TRIES):
        try:
            mock_get("/stats")
            return
        except RuntimeError as error:
            last = error
            time.sleep(0.5)
    raise RuntimeError(f"the mock upstream never answered /stats: {last}")


def busbar_dir():
    """Locate the pinned busbar source checkout used to build the packer."""
    supplied = os.environ.get("BUSBAR_DIR")
    candidate = supplied or os.path.join(os.path.dirname(REPO), "busbar")
    candidate = os.path.abspath(candidate)
    manifest = os.path.join(candidate, "crates", "plugin-loader", "Cargo.toml")
    if not os.path.isfile(manifest):
        sys.exit("set BUSBAR_DIR to a busbar checkout containing crates/plugin-loader")
    return candidate


def package_version(manifest):
    """The package version from Cargo.toml text; a manifest without one stops the pack rather
    than labelling the tarball with a made-up version."""
    # cheap TOML scrape (avoids a tomllib dep / import ordering fuss): version = "x.y.z"
    found = re.search(r'(?m)^version\s*=\s*"([^"]+)"', manifest)
    if not found:
        sys.exit("Cargo.toml has no package version to stamp into the plugin tarball")
    return found.group(1)


def prep_plugin():
    """Build the Headroom cdylib from THIS checkout and pack it as an unsigned dev tarball.

    Requires the host arch/OS to match the busbar container's (Linux) — see the module docstring.
    Writes plugins/busbar-hook-headroom.tar.gz, bind-mounted read-only at /etc/busbar/plugins by
    start_busbar() for the hook phase. config.hook.yaml sets plugins.trust.allow_unsigned: true to
    accept it; a real deployment installs busbar's release-CI-signed tarball instead.
    """
    print("building headroom-hook cdylib (cargo build --release)...", file=sys.stderr)
    sh("cargo", "build", "--release", "--locked", "--lib", cwd=REPO, quiet=False)

    ext = "dylib" if sys.platform == "darwin" else "so"
    lib = os.path.join(REPO, "target", "release", f"libbusbar_hook_headroom.{ext}")
    if not os.path.exists(lib):
        sys.exit(f"expected cdylib not found: {lib} (did the release build produce it?)")

    bdir = busbar_dir()
    print(f"building busbar-plugin-pack from {bdir} ...", file=sys.stderr)
    sh("cargo", "build", "--release", "--locked", "-p", "busbar-plugin-loader", "--features", "pack",
       "--bin", "busbar-plugin-pack", cwd=bdir, quiet=False)
    pack = os.path.join(bdir, "target", "release", "busbar-plugin-pack")
    if not os.path.exists(pack):
        sys.exit(f"busbar-plugin-pack binary not found at {pack}")

    with open(os.path.join(REPO, "Cargo.toml")) as manifest:
        version = package_version(manifest.read())

    shutil.rmtree(PLUGIN_DIR, ignore_errors=True)
    os.makedirs(PLUGIN_DIR, exist_ok=True)
    out = os.path.join(PLUGIN_DIR, "busbar-hook-headroom.tar.gz")
    sh(pack, "pack",
       "--lib", lib,
       "--name", "busbar-hook-headroom", "--alias", "headroom", "--kind", "hook",
       "--version", version, "--publisher", "busbar", "--license", "Apache-2.0",
       "--needs-prompt", "rw", "--allow-unsigned",
       "--declares-file", os.path.join(REPO, "declares.json"),
       "--out", out,
       quiet=False)
    if not os.path.exists(out):
        sys.exit(f"plugin pack did not produce {out}")
    print(f"packed {out}", file=sys.stderr)


def start_busbar(config, with_hook):
    rm(BUSBAR)
    args = ["docker", "run", "-d", "--name", BUSBAR, "--network", f"container:{MOCK}",
            "-e", "BENCH_MOCK_KEY=x",
            "-v", f"{HERE}/{config}:/etc/busbar/config.yaml:ro",
            "-v", f"{HERE}/providers.mock.yaml:/etc/busbar/providers.yaml:ro"]
    if with_hook:
        # The whole hook phase, vs. baseline, is: this one extra mount + config.hook.yaml's
        # `plugins:`/`hooks:`/`pools.hooks` blocks.
        args += ["-v", f"{PLUGIN_DIR}:/etc/busbar/plugins:ro"]
    args += [BUSBAR_IMAGE]
    sh(*args)
    if not wait_ready():
        print("busbar did not become ready; logs:", file=sys.stderr)
        sh("docker", "logs", BUSBAR, check=False, quiet=False)
        sys.exit(1)


def measure(config, with_hook, args):
    start_busbar(config, with_hook)
    body = openai_body(args.history_kb)
    mock_get("/reset")
    durs = load(body, args.requests, args.concurrency, args.warmup)
    stats = mock_get("/stats")
    if not stats.get("requests"):
        raise RuntimeError(f"{config}: no request reached the mock upstream, so there is no token count")
    tokens = round(stats.get("prompt_tokens_est", 0) / stats["requests"])
    return {
        "busbar_dur_us": {q: percentile(durs, p) for q, p in (("p50", .5), ("p90", .9), ("p99", .99))},
        "samples": len(durs),
        "tokens_per_req": tokens,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--requests", type=int, default=1000)
    ap.add_argument("--concurrency", type=int, default=1)
    ap.add_argument("--warmup", type=int, default=50)
    ap.add_argument("--history-kb", type=int, default=11)
    ap.add_argument("--delay-ms", type=int, default=0)
    ap.add_argument("--skip-plugin-build", action="store_true",
                     help="reuse an existing plugins/busbar-hook-headroom.tar.gz instead of rebuilding")
    args = ap.parse_args()
    if args.requests <= 0 or args.concurrency <= 0 or args.history_kb <= 0:
        ap.error("requests, concurrency and history-kb must be positive")
    if args.warmup < 0 or args.delay_ms < 0:
        ap.error("warmup and delay-ms must be nonnegative")
    if not BUSBAR_IMAGE:
        ap.error("set BUSBAR_IMAGE to a dynamically linked busbar image at .busbar-ref, "
                 "with the same libc and architecture as the plugin build")

    if not args.skip_plugin_build:
        prep_plugin()
    elif not os.path.exists(os.path.join(PLUGIN_DIR, "busbar-hook-headroom.tar.gz")):
        sys.exit("--skip-plugin-build given but plugins/busbar-hook-headroom.tar.gz does not exist")

    try:
        start_mock(args.delay_ms)
        base = measure("config.baseline.yaml", False, args)
        hook = measure("config.hook.yaml", True, args)
    finally:
        rm(MOCK, BUSBAR)

    delta = {q: (hook["busbar_dur_us"][q] - base["busbar_dur_us"][q])
             for q in ("p50", "p90", "p99")}
    saved_pct = round(100 * (1 - hook["tokens_per_req"] / max(1, base["tokens_per_req"])), 1)
    result = {
        "config": vars(args),
        "images": {"busbar": BUSBAR_IMAGE},
        "plugin": ("reused plugins/busbar-hook-headroom.tar.gz (--skip-plugin-build)"
                   if args.skip_plugin_build
                   else "built from this checkout, packed unsigned (dev mode)"),
        "baseline": base, "hook": hook,
        "added_busbar_dur_us": delta,
        "tokens": {"baseline": base["tokens_per_req"], "hook": hook["tokens_per_req"],
                   "saved_pct": saved_pct},
    }
    os.makedirs(f"{HERE}/results", exist_ok=True)
    with open(f"{HERE}/results/docker_ab.json", "w") as f:
        json.dump(result, f, indent=2)
    print(json.dumps(result, indent=2))
    print(f"\nADDED busbar;dur (hook cost)  p50={delta['p50']}µs "
          f"p90={delta['p90']}µs p99={delta['p99']}µs")
    print(f"tokens/req {base['tokens_per_req']} -> {hook['tokens_per_req']}  ({saved_pct}% saved)")


if __name__ == "__main__":
    main()
