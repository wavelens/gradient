# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

GAIN_PER_ADDED_WORKER = 0.15
SMALL_SLOWDOWN = 1.1
EVAL_PHASES = {"eval_flake", "eval_derivations"}
EVALUATION_REQUEST_TO_BUILDING_STARTED_MS_KEY = "evaluation_request_to_building_started_ms"
WORKERS_EVALUATION_PHASE_SPAN_MS_KEY = "workers_evaluation_phase_span_ms"


def evaluation_phase_span_milliseconds(phases):
    spans = [p for p in phases if p["phase"] in EVAL_PHASES]
    if not spans:
        return 0

    start = min(p.get("offset_ms", 0) + p["start_ms"] for p in spans)
    end = max(p.get("offset_ms", 0) + p["end_ms"] for p in spans)
    return end - start


def get_evaluation_duration_milliseconds(run):
    return run[EVALUATION_REQUEST_TO_BUILDING_STARTED_MS_KEY]


def speedup(runs, n):
    return get_evaluation_duration_milliseconds(runs[1]) / max(get_evaluation_duration_milliseconds(runs[n]), 1)


def target(n):
    return 1 + GAIN_PER_ADDED_WORKER * (n - 1)


def jobs_text(jobs):
    return f"{jobs} job" if jobs == 1 else f"{jobs} jobs"


def speedup_verdicts(runs):
    if 1 not in runs:
        return []

    failed = []
    for n in sorted(n for n in runs if n > 1):
        if speedup(runs, n) < target(n):
            failed.append(f"N={n}: speedup {speedup(runs, n):.2f} below {target(n):.2f}")
        if runs[n]["jobs"] <= 1:
            failed.append(f"N={n}: ran on {jobs_text(runs[n]['jobs'])}")

    return failed


def small_complete(small):
    return bool(small) and "standalone" in small and "multinode" in small


def small_verdicts(small):
    if not small_complete(small):
        return []

    standalone, multinode = small["standalone"], small["multinode"]
    failed = []
    if get_evaluation_duration_milliseconds(multinode) > SMALL_SLOWDOWN * get_evaluation_duration_milliseconds(standalone):
        failed.append(f"small flake: {get_evaluation_duration_milliseconds(multinode)} ms with 4 workers up,"
                      f" {get_evaluation_duration_milliseconds(standalone)} ms standalone")
    if multinode["jobs"] != 1:
        failed.append(f"small flake: ran on {jobs_text(multinode['jobs'])}")

    return failed


def verdicts(runs, small=None):
    return speedup_verdicts(runs) + small_verdicts(small)


def run_line(runs, n):
    run = runs[n]
    formatted_speedup_column_text = f"  {speedup(runs, n):5.2f}x" if 1 in runs else ""
    return (f"N={n}  end-to-end {get_evaluation_duration_milliseconds(run) / 1000:8.1f} s{formatted_speedup_column_text}"
            f"  worker phase {run[WORKERS_EVALUATION_PHASE_SPAN_MS_KEY] / 1000:8.1f} s"
            f"  {run['statements']:7} statements  {run['shards']:4} shards  {jobs_text(run['jobs'])}")


def render(runs, small):
    lines = [run_line(runs, n) for n in sorted(runs)]
    if small_complete(small):
        standalone, multinode = small["standalone"], small["multinode"]
        lines.append(f"small flake: end-to-end {get_evaluation_duration_milliseconds(standalone) / 1000:.1f} s standalone,"
                     f" {get_evaluation_duration_milliseconds(multinode) / 1000:.1f} s with 4 workers up on {jobs_text(multinode['jobs'])}"
                     f"  worker phase {standalone[WORKERS_EVALUATION_PHASE_SPAN_MS_KEY] / 1000:.1f} s standalone,"
                     f" {multinode[WORKERS_EVALUATION_PHASE_SPAN_MS_KEY] / 1000:.1f} s with 4 workers up")

    return "\n".join(lines) + "\n"
