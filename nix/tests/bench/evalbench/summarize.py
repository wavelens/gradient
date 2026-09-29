# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

# Merges the span files of one benchmark run onto the server's clock and reduces
# them to a per-stage table. Server and worker run on separate VMs, so the worker
# clock is shifted by the offset that makes both message directions symmetric.

import json
import pathlib
import statistics
import tarfile

SERVER_CLOCK = ("server",)


def load_spans(trace_dir):
    spans = []
    for path in sorted(pathlib.Path(trace_dir).glob("*.jsonl")):
        for line in path.read_text().splitlines():
            try:
                spans.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    return spans


def by_job(spans, process, name, kind=None):
    grouped = {}
    for span in sorted(spans, key=lambda s: s["ts_us"]):
        fields = span.get("fields", {})
        if span["process"] != process or span["name"] != name:
            continue
        if kind is not None and fields.get("kind") != kind:
            continue
        grouped.setdefault(fields.get("job_id"), []).append(span)
    return grouped


def upstream_delays(spans):
    sent = by_job(spans, "worker", "report_eval_result")
    received = by_job(spans, "server", "job_event", kind="eval_result")
    return [
        (recv["ts_us"] - recv["fields"].get("queue_wait_us", 0)) - send["ts_us"]
        for job_id, sends in sent.items()
        for send, recv in zip(sends, received.get(job_id, []))
    ]


def downstream_delays(spans):
    assigned = by_job(spans, "server", "assign_job")
    started = by_job(spans, "worker", "job")
    return [
        start["ts_us"] - (assign["ts_us"] + assign["dur_us"])
        for job_id, assigns in assigned.items()
        for assign, start in zip(assigns, started.get(job_id, []))
    ]


def clock_offset(spans):
    up, down = upstream_delays(spans), downstream_delays(spans)
    if not up or not down:
        return 0, False
    return (min(up) - min(down)) // 2, True


def lanes(spans):
    stacks, assigned = [], []
    for span in sorted(spans, key=lambda s: (s["ts"], -s["dur"])):
        start, end = span["ts"], span["ts"] + span["dur"]
        for lane, stack in enumerate(stacks):
            while stack and stack[-1] <= start:
                stack.pop()
            if not stack or stack[-1] >= end:
                stack.append(end)
                assigned.append((lane, span))
                break
        else:
            stacks.append([end])
            assigned.append((len(stacks) - 1, span))
    return assigned


def chrome_trace(spans, offset_us):
    processes = sorted({(s["process"], s["pid"]) for s in spans})
    events = []
    for index, (process, pid) in enumerate(processes, start=1):
        own = [
            {
                "name": s["name"],
                "cat": process,
                "ph": "X",
                "ts": s["ts_us"] + (0 if process in SERVER_CLOCK else offset_us),
                "dur": s["dur_us"],
                "pid": index,
                "args": s.get("fields", {}),
            }
            for s in spans
            if (s["process"], s["pid"]) == (process, pid)
        ]
        events.append({
            "ph": "M",
            "name": "process_name",
            "pid": index,
            "args": {"name": f"{process} ({pid})"},
        })
        events.extend({**span, "tid": lane} for lane, span in lanes(own))
    return {"traceEvents": events, "displayTimeUnit": "ms"}


def stage_table(spans):
    groups = {}
    for span in spans:
        groups.setdefault((span["process"], span["name"]), []).append(span["dur_us"] / 1000)
    rows = [
        {
            "process": process,
            "name": name,
            "count": len(durations),
            "total_ms": round(sum(durations), 3),
            "p50_ms": round(statistics.median(durations), 3),
            "max_ms": round(max(durations), 3),
        }
        for (process, name), durations in groups.items()
    ]
    return sorted(rows, key=lambda r: r["total_ms"], reverse=True)


def wall_ms(spans, offset_us):
    if not spans:
        return 0.0
    shifted = [
        (s["ts_us"] + (0 if s["process"] in SERVER_CLOCK else offset_us), s["dur_us"])
        for s in spans
    ]
    return round((max(t + d for t, d in shifted) - min(t for t, _ in shifted)) / 1000, 3)


def summarize_run(run_dir):
    run_dir = pathlib.Path(run_dir)
    spans = load_spans(run_dir / "trace")
    offset_us, aligned = clock_offset(spans)
    (run_dir / "trace.json").write_text(json.dumps(chrome_trace(spans, offset_us)))
    metric = run_dir / "evaluation_metric.json"
    return {
        "aligned": aligned,
        "offset_us": offset_us,
        "wall_ms": wall_ms(spans, offset_us),
        "spans": len(spans),
        "evaluation_metric": json.loads(metric.read_text()) if metric.exists() else [],
        "stages": stage_table(spans),
    }


def render(runs, top=40):
    lines = []
    for name, run in runs.items():
        alignment = "aligned" if run["aligned"] else "unaligned"
        lines.append(
            f"== {name}: wall {run['wall_ms'] / 1000:.2f} s, {run['spans']} spans, "
            f"worker offset {run['offset_us'] / 1000:+.1f} ms ({alignment})"
        )
        for metric in run["evaluation_metric"]:
            lines.append("   evaluation_metric " + " ".join(f"{k}={v}" for k, v in metric.items()))
        lines.append(f"   {'process':<8} {'span':<32} {'count':>7} {'total ms':>11} {'p50 ms':>9} {'max ms':>9}")
        for row in run["stages"][:top]:
            lines.append(
                f"   {row['process']:<8} {row['name']:<32} {row['count']:>7} "
                f"{row['total_ms']:>11.1f} {row['p50_ms']:>9.2f} {row['max_ms']:>9.2f}"
            )
        lines.append("")
    return "\n".join(lines)


def summarize(out_dir, runs):
    out_dir = pathlib.Path(out_dir)
    summaries = {name: summarize_run(out_dir / name) for name in runs}
    (out_dir / "summary.json").write_text(json.dumps(summaries, indent=2))
    (out_dir / "summary.txt").write_text(render(summaries))
    return summaries


def publish(out_dir):
    out_dir = pathlib.Path(out_dir).resolve()
    bundle = out_dir / "evalbench.tar.gz"
    with tarfile.open(bundle, "w:gz") as tar:
        for entry in sorted(out_dir.iterdir()):
            if entry.name not in ("nix-support", bundle.name):
                tar.add(entry, arcname=f"evalbench/{entry.name}")

    products = out_dir / "nix-support"
    products.mkdir(exist_ok=True)
    (products / "hydra-build-products").write_text(
        f"file tarball {bundle}\n"
        f"file text {out_dir / 'summary.txt'}\n"
        f"file json {out_dir / 'summary.json'}\n"
    )
