<!-- fleet:header:begin (rendered by `cargo xtask fleet render` from GetBusbar/busbar's plugins.yaml; edit it there) -->
# busbar-hook-headroom

Compresses LLM chat history with the real headroom-core BM25 TextCrusher engine as a prompt:rw rewrite gate: a busbar first-party kind:hook plugin (dlopen cdylib).

| kind | alias | crate | busbar | license |
|---|---|---|---|---|
| `hook` | `headroom` | `busbar-hook-headroom` | 1.6.0 (pinned in `.busbar-ref`) | Apache-2.0 |

<!-- fleet:header:end -->

## What it is for

[![Coverage](https://codecov.io/gh/GetBusbar/busbar-hook-headroom/branch/dev/graph/badge.svg)](https://codecov.io/gh/GetBusbar/busbar-hook-headroom)

**v2.** Compresses LLM chat history inside [busbar](https://getbusbar.com)
using [headroom](https://github.com/headroomlabs-ai/headroom)'s Rust
compression core (`TextCrusher`, pure BM25: no model, no network, no proxy).
A `kind: hook` `prompt: rw` rewrite gate, shipped as a signed `dlopen`
plugin busbar loads in-process over its plugin ABI (no standalone binary,
no socket); targets busbar 1.6.0 (plugin ABI v2, the rev pinned in
`.busbar-ref`) and pins a `headroom-core` release tag.

### Benchmark

Measured the way busbar measures itself: from **busbar's own clock**. Busbar reports
its internal processing time — total request time minus the upstream round-trip — in
a standard `Server-Timing: busbar;dur=<ms>` header on every response. The Headroom
gate runs *synchronously* inside that window (busbar calls the hook and waits for the
rewrite before dispatch), so `busbar;dur` captures exactly what the hook adds, on
busbar's clock, with none of the benchmark harness's own round-trip in it. Every
number below is reproducible — see [`bench/`](bench/README.md).

### Added latency

`busbar;dur`, in µs, concurrency 1, 1,000 requests per row, 11 KB noisy tool-log
history, openai → openai:

| | p50 | p90 | p99 |
|---|--:|--:|--:|
| Busbar alone | `22` | `25` | `30` |
| Busbar + Headroom | `569` | `601` | `634` |
| **Headroom's added cost** | **`547`** | **`576`** | **`604`** |

Cross-protocol (anthropic → openai) is within noise: `34` base / `584` with-hook /
`550` added, p50.

Two things this shows. **Busbar itself is tens of µs** (`22` µs p50 here; its own
[benchmark](https://getbusbar.com/docs/benchmark/) sweeps `37`–`93` µs across
protocols and payloads) — the gateway is not where your latency goes. And **Headroom
adds ~`547` µs** to compress an 11 KB history, with a **tight tail**: p99 is only
~1.1× p50, because busbar and the hook are single Rust binaries with no garbage
collector — nothing in the path pauses to sweep memory. Compression cost scales with
history size (in-process dlopen call, p50): `150` µs at 2 KB, `380` µs at 8 KB, `720` µs
at 16 KB, `2,900` µs at 64 KB.

On a two-second inference call, `547` µs is **0.03%** of the request.

### Token savings (context, not the headline)

This is **not** a compression benchmark — measuring how well Headroom compresses is
the [Headroom project's](https://headroomlabs-ai.github.io/headroom/) job, and it
reports higher ratios than the ~50% seen here (66–94% on some content types), with
more to gain from tuning the keep-ratio. What matters here is that the plumbing is
sound: the mock upstream tallies the tokens it actually received, so these are the
prompts that *really shipped* to the provider, not hook-side accounting.

| content | input tokens before | after | saved |
|---|--:|--:|--:|
| noisy tool log (11 KB) | `2,832` | `1,422` | **49.8%** |
| RAG dump (~16 KB) | `4,211` | `2,127` | **49.5%** |
| short chat | `22` | `22` | 0% (abstains) |

`bench/docker_ab.py` reproduces the tool-log row and the latency table (openai ingress). The
RAG-dump and short-chat rows, the abstain rates below, the cross-protocol figure and the
in-process timings by history size above were measured with an earlier version of the harness
that is no longer in `bench/`.

Short conversational chats pass through byte-identical (100% abstain over 100 short
chats; 0% over 100 compressible histories) — nothing to trim, nothing touched. And if
the hook is ever slow, wrong, or down, the request proceeds with its original body.

### Next to a proxy

Headroom's own HTTP proxy reports, from
[production telemetry](https://headroomlabs-ai.github.io/headroom/benchmarks/)
(50,000+ sessions, 250+ instances), a **52 ms median** overhead — which, as they
rightly note, is negligible against multi-second inference. Running the same
compression core as a busbar gate, the added cost measures **547 µs** on busbar's
clock. Both are small next to the model call; we make no claim about how their proxy
is deployed — only that the same core, run as an in-process dlopen gate on busbar's path, measures
in the hundreds of microseconds. Full credit to the
[Headroom](https://headroomlabs-ai.github.io/headroom/) project for the core; busbar
just puts it in front of every model you call.

Reproduce every number: see [`bench/README.md`](bench/README.md).

### Install and run

Headroom is a **busbar plugin**: a signed cdylib busbar `dlopen`s in-process (busbar's
plugin ABI). There is no standalone binary and no socket to wire up — busbar loads the
library directly and calls it as an in-process rewrite gate. There are two ways to
run it:

### 1. Bundled image (simplest — one container, zero config)

If you came to busbar specifically to run Headroom and just want it working, this is
the path: busbar + Headroom in one container, pre-installed and pre-wired.

```sh
docker run -d -p 8080:8080 \
  -e ANTHROPIC_KEY -e BUSBAR_ADMIN_TOKEN \
  getbusbar/busbar-headroom
```

Every request through the default pool is compressed immediately — no plugin install
step. See [`docker/bundle/config.yaml`](docker/bundle/config.yaml) for the baked-in
default config (one Anthropic provider, one `default` pool, Headroom attached to it as a
`prompt: rw` gate) and [`docker/bundle/Dockerfile`](docker/bundle/Dockerfile) for how
the image is assembled: it copies a busbar binary built from source with the same PGO
release build busbar's own official image uses, plus Headroom's signed cdylib, from a
staged build context (`binaries/<arch>/busbar`, `plugins/<arch>/`, `providers.yaml`); the
workflow that staged them is retired, so stage them yourself before `docker build`. Mount your own `config.yaml` over `/etc/busbar/config.yaml` to
replace the default entirely; copy the `plugins:`/`hooks:`/`pools.*.hooks`/`limits:` blocks from
the baked-in default into yours to keep Headroom wired.

This image is distinct from `getbusbar/busbar` (busbar's own plugin-free image) and
supersedes the old `getbusbar/headroom-hook` standalone image, which predates busbar's
dlopen plugin ABI and is no longer built.

### 2. Plugin drop-in (if you already run busbar)

If you already have a busbar deployment — [`getbusbar/busbar`](https://github.com/GetBusbar/busbar)
or your own build — install Headroom into it the same way you'd install any other
first-party plugin:

1. Grab the signed tarball for your platform from this repo's
   [Releases](https://github.com/GetBusbar/busbar-hook-headroom/releases/latest)
   (`busbar-headroom-<version>-<target>.tar.gz` — Linux x86_64/arm64, macOS
   x86_64/arm64, Windows x86_64).
2. Drop it into busbar's plugin directory and enable plugins in your `config.yaml`:

   ```yaml
   plugins:
     enabled: true
     dir: /etc/busbar/plugins   # or wherever you unpacked the tarball
   ```
3. Wire it in as a hook wherever you want compression — globally or per-pool:

   ```yaml
   hooks:                    # the top-level definition map: the NAME is the instance
     headroom:
       module: headroom          # the plugin alias (the release manifest is named busbar-headroom)
       kind: gate
       prompt: rw            # the rewrite grant
       timeout_ms: 25        # ~550 µs typical; 25 ms is generous headroom
       on_error: nothing     # a broken compressor never touches a request
       settings:
         target_ratio: 0.5
         min_savings_pct: 10

   pools:
     hooks: [headroom]       # the reserved all-pools attach (or list it per-pool)
   ```

The tarball is signed with busbar's release key, which busbar's release binaries embed,
so it loads as first-party with no extra trust configuration.

### Bound what the hook is shown

Set `limits.hook_content_max_bytes` (the shipped configs use 1 MiB). busbar's default is
unlimited, and TextCrusher holds roughly 50x a message's size in memory while it works, on a
thread busbar's `timeout_ms` cannot cancel: an 8 MB message peaked near 390 MB. Over the cap
busbar shows the hook an empty projection and the gate abstains. The cap is global, applying to
every `prompt: ro|rw` hook: a redaction or guardrail hook that must see every prompt fails open
behind it, so raise or remove the cap if you run one. Compression time also grows
with (history sentences) x (distinct words in the last message): a 54 KB history behind a
23 KB final message took about 4.7 s of CPU, so the cap bounds that cost but does not make it
small.

### Build from source

Needs a Rust toolchain ([rustup](https://rustup.rs)); the pinned `headroom-core` tag is
in `Cargo.toml`. This builds the cdylib only (`crate-type = ["cdylib", "rlib"]` — there
is no `[[bin]]`, no standalone executable), which you then sign/pack with busbar's own
`busbar-plugin-pack` tool before busbar will load it as first-party (or load it
unsigned in dev mode via `plugins.trust.allow_unsigned`):

```sh
git clone https://github.com/GetBusbar/busbar-hook-headroom && cd busbar-hook-headroom
cargo build --release --lib   # cdylib: target/release/libbusbar_hook_headroom.so (.dylib/.dll elsewhere)
```

### Settings

`target_ratio` (default 0.5, roughly the fraction of each history message's characters to keep), `min_savings_pct`
(default 10, abstain below this saving), and `price_udollars_per_ktok` (default 2500,
the $-estimate price) SEED the startup values — once busbar pushes settings live
(over the admin API's hook-settings endpoint, or the `settings:` block in
`config.yaml` above), the push wins. See [Metrics](#metrics) below for the matching
status/metrics surface.

**OS support.** As a `dlopen`'d cdylib, Headroom runs wherever busbar itself can load
a plugin built for the target platform — Linux and macOS natively; on Windows, build
and load it inside WSL2 or a Linux container alongside busbar.

### The plugin ABI (busbar's `HookHandler` calls)

No wire, no serialization — busbar calls the loaded plugin in-process through the SDK's
[`HookHandler`](https://github.com/GetBusbar/busbar) trait:

| call | when | this hook's response |
|---|---|---|
| `configure` | on a settings PATCH (busbar 1.6.0 passes the config-file `settings:` to `open` at load) | acks with the applied `settings_version` — only if the settings applied cleanly (commit-on-ack) |
| `describe` | any time | `{schema, dashboard}` — the settings JSON Schema (served at `GET /api/v1/admin/hooks/headroom/schema`) + a dashboard widget layout that busbar 1.6.0 does not surface |
| `status` | any time | `{status:{settings, metrics:[…]}}` — observed settings + self-reported metrics (see [Metrics](#metrics)) |
| `transform` | per request | `{"rewrite":{...}}` or `{}` (abstain) |
| `decide` | per request | always `{}` (abstain): Headroom ranks nothing |
| `notify` | fire-and-forget (taps only) | none read |

What `transform` keeps verbatim: the last message (the ask) and any `system` or `developer`
turn that arrives among the turns. It abstains (and still counts the request) when the last
turn is not a `user` turn (an assistant prefill is not the ask), and when busbar has hoisted
a system prompt into `request.system` on a dialect that can carry it inside the turns array
(OpenAI chat, OpenAI Responses, Cohere): busbar 1.6.0 replaces that whole array with the
rewrite, so a commit would ship the request without its system prompt. On Responses the
hoisted text may also be the top-level `instructions`, which would survive, but the
projection cannot tell the two apart, so Responses requests with instructions are not
compressed. Anthropic, Bedrock and Gemini keep the system prompt in a body field of its own
and compress normally. It
also abstains when busbar projects a turn with the role `tool` (or
`function`), which it does for tool results on OpenAI chat, OpenAI Responses and Cohere:
busbar's projection is text-only and the rewrite replaces the whole messages array, so a
rewrite there would drop the `tool_call_id`/`tool_calls` structure the provider requires.
On Anthropic, Bedrock and Gemini, tool calls and results are blocks inside ordinary
`user`/`assistant` turns, so the hook cannot see them: a committed rewrite flattens them
to plain text (the provider accepts it, but the tool exchange becomes prose). Images and
other non-text blocks inside a turn are likewise invisible to the projection and do not
survive a committed rewrite. busbar documents this rewrite limitation (its `docs/hooks.md`,
the `rewrite` arm).

Settings (`target_ratio`, `min_savings_pct`, `price_udollars_per_ktok`)
arrive as desired state: a key absent from the push resets to its default, a
rejected push (unknown key, bad value) is never acked and busbar keeps the
previous settings. For a hook instance created through the admin API, retune it live, no
restart (the body wraps the map in `settings`):

```sh
curl -X PATCH localhost:8080/api/v1/admin/hooks/headroom/settings \
  -H "Authorization: Bearer $BUSBAR_ADMIN_TOKEN" -H 'content-type: application/json' \
  -d '{"settings": {"target_ratio": 0.3, "min_savings_pct": 20}}'
```

busbar refuses (409) to patch a hook defined in the base `config.yaml`, which is how every
example in this README defines it: edit its `settings:` block in `config.yaml` instead.

### Metrics

The hook reports its own operational metrics on the `status` call, and busbar surfaces them two
ways from that one source:

- **Live JSON** — `GET /api/v1/admin/hooks/headroom/status` queries the hook on the spot (you set the
  resolution by how often you poll: hit it every second, get one-second-fresh data).
- **Prometheus** — busbar's `/metrics/hooks` scrape renders the same metrics as standard text, with
  the metric **names verbatim** and an automatic `hook="<instance>"` label (`headroom` in these examples,
  `busbar-headroom` in the bundled image and `config.example.yaml`).

The series emitted, named exactly as they appear on the scrape:

- `headroom_requests_total` — requests this hook saw, per pool.
- `headroom_tokens_input_total` — estimated input tokens sent upstream (after any rewrite), Headroom's
  post-compression definition, so `saved / (input + saved)` is the reduction.
- `headroom_tokens_saved_total` — estimated tokens removed by a committed rewrite.
- `headroom_overhead_ms_sum` / `_count` / `_min` / `_max` — the hook's own wall-clock overhead.
  Counters and gauges, NOT a histogram: there are no quantile buckets to query.
- `dollars_saved` — an estimate priced off `price_udollars_per_ktok`, not measured spend. busbar's
  `/usage` is the measured figure; use that for anything billing-facing.

Savings are booked when the hook commits a rewrite. If busbar's `timeout_ms` has already given up on
that call (busbar cannot cancel the thread, so the hook still finishes), the original body was sent,
yet the rewrite is still counted: on slow, very large prompts tokens saved and dollars saved
over-report, and input tokens under-report.

Every series carries a `pool` label, so one instance serving N pools shows N rows. The counters live
in the plugin instance: busbar opens a fresh instance on a config reload or a settings PATCH, and the
series restart from zero then.

Two caveats worth stating rather than discovering. The token and dollar figures are derived from a
BYTE count divided by a chars-per-token constant, so on text that is not mostly ASCII (CJK, emoji,
any script above the Latin range) they over-report by roughly the UTF-8 expansion factor. The
savings percentage is also a byte-based estimate; a change in language or script mix
can change its relationship to actual token savings. And the
`headroom_*` names mirror Headroom's own `/metrics` exposition verbatim, so a dashboard built for
Headroom reads them off busbar unmodified (see `grafana/README.md`).

### Wire into busbar (fleet-wide)

Plugins are enabled once; a hook instance is DEFINED ONCE in the top-level `hooks:` map (the name
is the instance, `module:` is the plugin behind it) and REFERENCED BY BARE NAME wherever it
attaches — there is no separate registry block:

```yaml
plugins:
  enabled: true
  dir: /etc/busbar/plugins

hooks:
  headroom:
    module: headroom
    kind: gate
    prompt: rw                 # the rewrite grant
    timeout_ms: 25             # ~550 µs typical; 25 ms is generous headroom
    on_error: nothing          # a broken compressor never touches a request
    settings:                  # handed to the plugin when busbar opens it
      target_ratio: 0.5
      min_savings_pct: 10

pools:
  hooks: [headroom]            # reserved all-pools attach: fires on every request, in order
```

### A/B test it (same plugin, two pools)

The clean experiment: one busbar, two pools over the same model — one with
the hook, one without — and point half your traffic at each. Compare
per-pool tokens and latency in `/metrics` or `GET /api/v1/admin/usage`.

```yaml
hooks:
  headroom: { module: headroom, kind: gate, prompt: rw, timeout_ms: 25, on_error: nothing }

pools:
  # NB: no `pools.hooks:` here — the all-pools attach would put the gate on both arms.
  with-headroom:
    hooks: [headroom]
    members: [ { model: claude-sonnet, weight: 1 } ]
  baseline:
    members: [ { model: claude-sonnet, weight: 1 } ]
```

> **Status:** pool-scoped rewrite gates were not fired by the pre-1.5.0 engine these
> numbers were benchmarked against (the transform pass was global-only at the time),
> and on same-protocol passthrough the engine's pristine-bytes fast path can skip the
> rewritten body. Both are tracked upstream.
> If your busbar build still has that limitation, A/B with two busbar instances (one
> with an all-pools `pools.hooks` attach, one without) instead, which is how the numbers above
> were measured. Check `busbar --version` / release notes for the fix.

[`scripts/docker-smoke.sh`](scripts/docker-smoke.sh) is the release-gate check: it
builds and packs the plugin from this checkout, `dlopen`s it into a real busbar container
(a dynamically linked busbar image at `.busbar-ref`, matching the plugin build's libc and
architecture), and proves the rewrite actually reaches the upstream
(fewer tokens shipped than arrived) — the class of failure `cargo test` alone can't
catch (manifest/ABI load failures, a silent no-op gate).
The `bench/` directory has the full measurement rig and results.

## Config

Configured as `module: headroom` (the plugin alias) under a hook instance in the top-level
`hooks:` map. Use the alias: the signed release tarball's manifest is named `busbar-headroom`
(busbar's plugin registry), while a build that links this crate, or a dev pack from `bench/`,
names it `busbar-hook-headroom`; both carry the alias `headroom`. The examples in this README
name the instance `headroom`; the bundled image, `config.example.yaml` and the bench configs
name it `busbar-headroom`. The instance name is what busbar uses in the admin URLs
(`/api/v1/admin/hooks/<instance>/...`) and in the `hook="<instance>"` metric label, so
substitute yours.

## Build

```bash
cargo build --release -p busbar-hook-headroom
```

## Tests

```bash
cargo test --workspace --locked
```

## License

Apache-2.0. See [LICENSE](LICENSE).

## Differences from 1.5.5

Invalid configuration and settings diagnostics no longer echo supplied keys or
values.
Known setting names and numeric bounds remain available for troubleshooting without
copying accidentally supplied credentials into operator logs.

Request and input counters now include granted prompts with zero or one message;
these requests still pass through without a rewrite.
