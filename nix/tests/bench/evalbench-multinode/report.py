# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

SPEEDUP_PER_WORKER = 0.8
SMALL_SLOWDOWN = 1.1
STATEMENTS_PER_SHARD = 10
EVAL_PHASES = {"eval_flake", "eval_derivations"}


def wall_ms(phases):
    spans = [p for p in phases if p["phase"] in EVAL_PHASES]
    if not spans:
        return 0

    start = min(p.get("offset_ms", 0) + p["start_ms"] for p in spans)
    end = max(p.get("offset_ms", 0) + p["end_ms"] for p in spans)
    return end - start


def speedup(runs, n):
    return runs[1]["wall_ms"] / max(runs[n]["wall_ms"], 1)


def jobs_text(jobs):
    return f"{jobs} job" if jobs == 1 else f"{jobs} jobs"


def speedup_verdicts(runs):
    failed = []
    for n in sorted(n for n in runs if n > 1):
        target = SPEEDUP_PER_WORKER * n
        if speedup(runs, n) < target:
            failed.append(f"N={n}: speedup {speedup(runs, n):.2f} below {target:.2f}")
        if runs[n]["jobs"] <= 1:
            failed.append(f"N={n}: ran on {jobs_text(runs[n]['jobs'])}")

    return failed


def statement_verdicts(runs):
    failed = []
    for n in sorted(runs):
        extra = runs[n]["statements"] - runs[1]["statements"]
        if extra > STATEMENTS_PER_SHARD * runs[n]["shards"]:
            failed.append(f"N={n}: {extra} extra statements for {runs[n]['shards']} shards")

    return failed


def small_verdicts(small):
    if not small:
        return []

    standalone, multinode = small["standalone"], small["multinode"]
    failed = []
    if multinode["wall_ms"] > SMALL_SLOWDOWN * standalone["wall_ms"]:
        failed.append(f"small flake: {multinode['wall_ms']} ms with 4 workers up,"
                      f" {standalone['wall_ms']} ms standalone")
    if multinode["jobs"] != 1:
        failed.append(f"small flake: ran on {jobs_text(multinode['jobs'])}")

    return failed


def verdicts(runs, small=None):
    return speedup_verdicts(runs) + statement_verdicts(runs) + small_verdicts(small)


def run_line(runs, n):
    run = runs[n]
    return (f"N={n}  {run['wall_ms'] / 1000:8.1f} s  {speedup(runs, n):5.2f}x"
            f"  {run['statements']:7} statements  {run['shards']:4} shards  {jobs_text(run['jobs'])}")


def render(runs, small):
    lines = [run_line(runs, n) for n in sorted(runs)]
    if small:
        standalone, multinode = small["standalone"], small["multinode"]
        lines.append(f"small flake: {standalone['wall_ms'] / 1000:.1f} s standalone,"
                     f" {multinode['wall_ms'] / 1000:.1f} s with 4 workers up on {jobs_text(multinode['jobs'])}")

    return "\n".join(lines) + "\n"
