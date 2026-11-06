#!/usr/bin/env python3
# Copyright (C) 2026 Busbar Inc and contributors
#
# Deterministic benchmark corpus for the headroom-hook rig: one realistic history flavor:
#
#   * tool_log(kb)   — noisy CI/deploy tool output: timestamped INFO noise with a few
#                      load-bearing WARN/ERROR lines buried in it. The "agent pasted a log" shape.
#
# Everything is formulaic — two runs of the rig produce byte-identical inputs.

TOOL_LOG_QUERY = "why did the deployment fail"

_SERVICES = ["api-edge", "billing", "search", "ingest", "webhooks", "notifier", "scheduler"]
_STEPS = [
    "pulled image layer sha256:{h} in 0.{ms}s",
    "helm release {svc} upgraded to revision {i} without incident",
    "readiness probe for pod {svc}-{i} passed after 2 attempts",
    "config map {svc}-cm rendered (checksum {h})",
    "autoscaler held {svc} at 3 replicas (cpu 41%)",
    "migration step {i} applied cleanly in 84ms",
    "cache warmed for shard {i} ({ms} keys)",
]


def tool_log(target_bytes: int) -> str:
    """A deploy log of ~target_bytes: mostly routine noise, one ERROR + two WARNs buried mid-way."""
    lines, i = [], 0
    size = 0
    # Positions for the load-bearing lines are fixed fractions of the (approximate) line count.
    approx_lines = max(6, target_bytes // 78)
    marks = {
        approx_lines // 2: "ERROR: deployment canary failed with status 503 on us-east-1.",
        approx_lines // 3: "WARN: rollout paused; canary error budget at 92% consumption.",
        (2 * approx_lines) // 3: "WARN: upstream health check flapping on api-edge after rollout.",
    }
    while size < target_bytes:
        if i in marks:
            line = f"14:02:{i % 60:02d} {marks[i]}"
        else:
            t = _STEPS[i % len(_STEPS)]
            line = "14:02:%02d INFO %s" % (
                i % 60,
                t.format(
                    h=format((i * 2654435761) % 0xFFFFFF, "06x"),
                    ms=100 + (i * 37) % 900,
                    svc=_SERVICES[i % len(_SERVICES)],
                    i=i,
                ),
            )
        lines.append(line)
        size += len(line) + 1
        i += 1
    return "\n".join(lines)


def history_messages(flavor: str, target_bytes: int):
    """Two history messages (each ~half the budget) + the short ask, as {role, content} dicts."""
    if flavor == "tool_log":
        half = tool_log(target_bytes // 2)
        ask = TOOL_LOG_QUERY
    else:
        raise ValueError(flavor)
    return [
        {"role": "user", "content": half},
        {"role": "assistant", "content": half},
        {"role": "user", "content": ask},
    ]
