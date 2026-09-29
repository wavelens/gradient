# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""Writes a self-contained `index.html` plus one standalone `.svg` per chart
and run, so the page survives being downloaded on its own."""

from __future__ import annotations

import base64
import pathlib
import shutil
from html import escape

from . import explain, svg
from .bundle import Run
from .flame import fold

METRICS = ("fetch_ms", "eval_drv_ms", "total_eval_ms", "total_thunks", "peak_rss_mb")

CSS = """
:root { --bg: #fbfbfa; --fg: #1d1d1f; --muted: #6b6b70; --line: #e2e2e4; --card: #ffffff; --ink: #111; }
@media (prefers-color-scheme: dark) {
  :root { --bg: #141416; --fg: #e8e8ea; --muted: #9a9aa2; --line: #2c2c31; --card: #1c1c20; --ink: #111; }
}
* { box-sizing: border-box; }
body { margin: 0 auto; max-width: 1280px; padding: 24px 16px 64px; background: var(--bg); color: var(--fg);
  font: 14px/1.45 system-ui, sans-serif; }
h1 { font-size: 22px; margin: 0 0 4px; } h2 { font-size: 18px; margin: 32px 0 8px; }
h3 { font-size: 14px; margin: 20px 0 6px; color: var(--muted); text-transform: uppercase; letter-spacing: .04em; }
.muted { color: var(--muted); }
.card { background: var(--card); border: 1px solid var(--line); border-radius: 8px; padding: 12px; overflow-x: auto; }
table { border-collapse: collapse; width: 100%; font-variant-numeric: tabular-nums; }
th, td { padding: 4px 8px; border-bottom: 1px solid var(--line); text-align: right; vertical-align: top; }
th:first-child, td:first-child, td.q { text-align: left; }
td.q { font-family: ui-monospace, monospace; font-size: 12px; max-width: 720px; }
pre { white-space: pre-wrap; font-size: 12px; margin: 6px 0; }
details > summary { cursor: pointer; }
nav a { margin-right: 12px; }
svg .label, svg .axis { fill: var(--fg); } svg .axis { fill: var(--muted); font-size: 10px; }
svg .grid { stroke: var(--line); } svg .ink { fill: var(--ink); pointer-events: none; }
svg rect:hover { stroke: var(--fg); stroke-width: 1; }
.key { display: inline-flex; align-items: center; margin: 0 12px 4px 0; font-size: 12px; }
.key i { width: 10px; height: 10px; border-radius: 2px; margin-right: 4px; display: inline-block; }
object { width: 100%; min-height: 420px; border: 0; }
"""


def _table(head: list[str], rows: list[list[str]]) -> str:
    return (
        "<table><tr>"
        + "".join(f"<th>{escape(h)}</th>" for h in head)
        + "</tr>"
        + "".join("<tr>" + "".join(rows_cell for rows_cell in row) + "</tr>" for row in rows)
        + "</table>"
    )


def _td(value, cls: str = "") -> str:
    attr = f' class="{cls}"' if cls else ""
    return f"<td{attr}>{escape(str(value))}</td>"


def _query(text: str) -> str:
    short = " ".join(text.split())
    if len(short) <= 110:
        return f'<td class="q">{escape(short)}</td>'
    return f'<td class="q"><details><summary>{escape(short[:110])}...</summary><pre>{escape(text)}</pre></details></td>'


def stage_totals(run: Run) -> dict[tuple[str, str], tuple[int, float]]:
    totals: dict[tuple[str, str], tuple[int, float]] = {}
    for span in run.spans:
        key = (span.process.split(" ")[0], span.name)
        count, total = totals.get(key, (0, 0.0))
        totals[key] = (count + 1, total + span.dur_us)
    return totals


def overview(runs: list[Run]) -> str:
    rows = []
    for run in runs:
        mode = f"{'cold' if run.meta.get('cold') else 'warm'}, {'instrumented' if run.meta.get('instrumented') else 'clean'}"
        rows.append(
            [f'<td><a href="#{run.name}">{escape(run.name)}</a></td>', _td(mode), _td(run.meta.get("evaluated_s", "")),
             _td(f"{run.wall_ms / 1000:.2f}"), _td(len(run.spans))]
            + [_td(run.metrics.get(m, "")) for m in METRICS]
        )
    return _table(["run", "mode", "evaluated s", "traced wall s", "spans", *METRICS], rows)


def comparison(runs: list[Run], top: int = 25) -> str:
    totals = {run.name: stage_totals(run) for run in runs}
    keys = sorted({k for t in totals.values() for k in t}, key=lambda k: -max(t.get(k, (0, 0))[1] for t in totals.values()))
    rows = [
        [_td(f"{process} {name}")]
        + [_td(f"{totals[r.name][(process, name)][1] / 1000:.1f}" if (process, name) in totals[r.name] else "") for r in runs]
        for process, name in keys[:top]
    ]
    return _table(["span (total ms)", *[r.name for r in runs]], rows)


def _chart(out: pathlib.Path, name: str, chart: str) -> str:
    if not chart:
        return '<p class="muted">no data</p>'
    (out / f"{name}.svg").write_text(chart)
    return f'<div class="card">{chart}</div><p class="muted"><a href="{out.name}/{name}.svg">{name}.svg</a></p>'


def _phase_totals(run: Run) -> list[tuple[str, float, str]]:
    totals: dict[str, float] = {}
    for job in run.jobs:
        for p in job.phases:
            key = f"{job.kind} {p.name}"
            totals[key] = totals.get(key, 0.0) + (p.end_ms - p.start_ms) * 1000
    return sorted(((k, v, "") for k, v in totals.items()), key=lambda r: -r[1])


def statements(run: Run, top: int = 30) -> str:
    rows = sorted(run.statements, key=lambda s: -float(s["total_ms"]))[:top]
    if not rows:
        return '<p class="muted">no pg_stat_statements</p>'
    chart = svg.bars([(f"#{i}", float(s["total_ms"]) * 1000, " ".join(s["query"].split())[:300]) for i, s in enumerate(rows)])
    table = _table(
        ["#", "calls", "total ms", "mean ms", "rows", "blks hit", "blks read", "query"],
        [[_td(i), _td(s["calls"]), _td(s["total_ms"]), _td(s["mean_ms"]), _td(s["rows"]), _td(s["shared_blks_hit"]),
          _td(s["shared_blks_read"]), _query(s["query"])] for i, s in enumerate(rows)],
    )
    return f'<div class="card">{chart}</div><div class="card">{table}</div>'


def explained(run: Run) -> str:
    if run.explain_log is None:
        return '<p class="muted">no auto_explain log (clean run)</p>'
    result = explain.parse(run.explain_log)
    by_query = _table(
        ["plans", "total ms", "max ms", "query"],
        [[_td(t.count), _td(f"{t.total_ms:.1f}"), _td(f"{t.max_ms:.1f}"), _query(t.query)] for t in result.by_query],
    )
    slowest = "".join(
        f"<details><summary>{p.duration_ms:.1f} ms - {escape(' '.join(p.query.split())[:140])}</summary>"
        f"<pre>{escape(p.text)}</pre></details>"
        for p in result.slowest
    )
    return (
        f'<p class="muted">{result.plans} plans logged</p>'
        f'<h3>Statements by total plan time</h3><div class="card">{by_query}</div>'
        f'<h3>Slowest plans</h3><div class="card">{slowest}</div>'
    )


def flames(run: Run, out: pathlib.Path) -> str:
    if not run.flames:
        return '<p class="muted">no perf flame graphs (clean run)</p>'
    parts = []
    for label, path in run.flames.items():
        target = out / f"perf-{label}.svg"
        shutil.copyfile(path, target)
        inline = base64.b64encode(path.read_bytes()).decode()
        parts.append(
            f'<h3>perf {escape(label)}</h3><object type="image/svg+xml" data="data:image/svg+xml;base64,{inline}"></object>'
            f'<p class="muted"><a href="{out.name}/{target.name}">{target.name}</a></p>'
        )
    return "".join(parts)


def section(run: Run, out_dir: pathlib.Path) -> str:
    out = out_dir / run.name
    out.mkdir(parents=True, exist_ok=True)
    trace = run.path / "trace.json"
    if trace.exists():
        shutil.copyfile(trace, out / "trace.json")
    stages = sorted(stage_totals(run).items(), key=lambda kv: -kv[1][1])[:30]
    stage_rows = [(f"{p} {n}", total, f"{p} {n}: {svg.ms(total)} in {count}") for (p, n), (count, total) in stages]
    phase_names = sorted({p.name for job in run.jobs for p in job.phases})
    return f"""
<h2 id="{run.name}">{escape(run.name)}</h2>
<p class="muted">{escape(" ".join(f"{k}={v}" for k, v in run.meta.items()))} -
open <a href="{run.name}/trace.json">trace.json</a> in <a href="https://ui.perfetto.dev">ui.perfetto.dev</a> to zoom.</p>
<h3>Job phases</h3>{_chart(out, "phase-totals", svg.bars(_phase_totals(run)))}
<div>{svg.legend(phase_names)}</div>{_chart(out, "phases", svg.phases(run.jobs))}
<h3>Span timeline</h3>{_chart(out, "timeline", svg.timeline(run.spans))}
<h3>Span flame graph</h3>{_chart(out, "span-flame", svg.icicle(fold(run.spans)))}
<h3>Span totals</h3>{_chart(out, "span-totals", svg.bars(stage_rows))}
<h3>pg_stat_statements</h3>{statements(run)}
<h3>auto_explain</h3>{explained(run)}
<h3>CPU flame graphs</h3>{flames(run, out)}
"""


def render(runs: list[Run], out_dir: pathlib.Path, source: str) -> pathlib.Path:
    out_dir.mkdir(parents=True, exist_ok=True)
    sections = "".join(section(run, out_dir) for run in runs)
    nav = "".join(f'<a href="#{r.name}">{escape(r.name)}</a>' for r in runs)
    page = f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Eval Benchmark Report</title><style>{CSS}</style></head><body>
<h1>Eval benchmark</h1><p class="muted">{escape(source)}</p><nav>{nav}</nav>
<h2>Runs</h2><div class="card">{overview(runs)}</div>
<h2>Span totals across runs</h2><div class="card">{comparison(runs)}</div>
{sections}
</body></html>"""
    index = out_dir / "index.html"
    index.write_text(page)
    return index
