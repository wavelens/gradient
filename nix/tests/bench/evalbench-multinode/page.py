# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

import json
import pathlib
from html import escape

from gradient_evalbench import svg
from gradient_evalbench.bundle import Span
from gradient_evalbench.flame import fold
from gradient_evalbench.report import CSS

import report

SHARD_STATES = {0: "queued", 1: "running", 2: "done", 3: "failed", 4: "split"}
BUSY = ("user", "nice", "system", "irq", "softirq")
TIMELINE_MIN_US = 1000

LANE_LABEL = svg.LABEL
PLOT = svg.WIDTH - svg.LABEL - 200


def seconds(ms):
    return f"{ms / 1000:.1f} s"


def lane_name(member):
    return "primary" if member == "primary" else f"member {member}"


def speedup_chart(runs):
    peak = max([report.speedup(runs, n) for n in runs] + [report.target(n) for n in runs])
    body = []
    for i, n in enumerate(sorted(runs)):
        y = i * (svg.ROW + 6)
        value, target = report.speedup(runs, n), report.target(n)
        fill = "#3fa66b" if value >= target else "#d0533f"
        body.append(svg._text(LANE_LABEL - 6, y + 13, f"N={n}", "end"))
        body.append(svg._rect(LANE_LABEL, y + 2, value / peak * PLOT, svg.ROW, fill,
                              f"N={n}: {value:.2f}x, target {target:.2f}x"))
        x = LANE_LABEL + target / peak * PLOT
        body.append(f'<line x1="{x:.2f}" y1="{y}" x2="{x:.2f}" y2="{y + svg.ROW + 4}" stroke="currentColor" stroke-width="2"/>')
        body.append(svg._text(LANE_LABEL + value / peak * PLOT + 6, y + 13, f"{value:.2f}x  target {target:.2f}x"))
    return svg._svg(len(runs) * (svg.ROW + 6) + 4, body)


def lanes_chart(lanes, bars):
    if not bars:
        return ""
    span_ms = max(start + length for _, start, length, _, _ in bars) or 1
    plot = svg.WIDTH - LANE_LABEL - 10
    row_of = {lane: i for i, lane in enumerate(lanes)}
    height = svg.AXIS + len(lanes) * svg.ROW + 4
    body = svg._axis(0, span_ms * 1000, height, plot)
    for lane, i in row_of.items():
        body.append(svg._text(LANE_LABEL - 6, svg.AXIS + i * svg.ROW + 12, lane, "end"))
    for lane, start, length, name, tip in bars:
        x = LANE_LABEL + start / span_ms * plot
        body.append(svg._rect(x, svg.AXIS + row_of[lane] * svg.ROW + 1, length / span_ms * plot, svg.ROW - 2,
                              svg.color(name), tip))
    return svg._svg(height, body)


def shard_chart(shards):
    taken = [s for s in shards if s.get("taken_ms") is not None and s.get("took_ms") is not None]
    members = sorted({s["member"] for s in taken}, key=lambda m: (m != "primary", m))
    bars = [(lane_name(s["member"]), s["taken_ms"], s["took_ms"], lane_name(s["member"]),
             f"#{s['seq']} {s['pattern']}: {s['children']} children, predicted {seconds(s['predicted_ms'])},"
             f" took {seconds(s['took_ms'])}")
            for s in taken]
    return lanes_chart([lane_name(m) for m in members], bars)


def phase_chart(phases):
    jobs = sorted({p["job"] for p in phases})
    bars = [(p["job"], p["offset_ms"] + p["start_ms"], p["end_ms"] - p["start_ms"], p["phase"],
             f"{p['phase']} {seconds(p['end_ms'] - p['start_ms'])} @ {seconds(p['offset_ms'] + p['start_ms'])}")
            for p in phases]
    return lanes_chart(jobs, bars)


def table(head, rows):
    cells = "".join(
        "<tr>" + "".join(f'<td class="q">{escape(str(c))}</td>' if i == 0 and len(str(c)) > 60 else f"<td>{escape(str(c))}</td>"
                         for i, c in enumerate(row)) + "</tr>"
        for row in rows
    )
    return "<table><tr>" + "".join(f"<th>{escape(h)}</th>" for h in head) + "</tr>" + cells + "</table>"


def prediction_chart(shards):
    done = [s for s in shards if s.get("took_ms") is not None]
    if not done:
        return ""
    peak = max(max(s["predicted_ms"] or 0, s["took_ms"]) for s in done) or 1
    body = []
    for i, s in enumerate(done):
        y = i * (svg.ROW + 2)
        ratio = s["took_ms"] / max(s["predicted_ms"] or 0, 1)
        tip = f"#{s['seq']} {s['pattern']}: predicted {seconds(s['predicted_ms'] or 0)}, took {seconds(s['took_ms'])} ({ratio:.2f}x)"
        body.append(svg._text(LANE_LABEL - 6, y + 12, f"#{s['seq']} {s['pattern']}"[:30], "end"))
        body.append(svg._rect(LANE_LABEL, y + 2, (s["predicted_ms"] or 0) / peak * PLOT, svg.ROW - 2, "#b8b8c0", tip))
        body.append(svg._rect(LANE_LABEL, y + 6, s["took_ms"] / peak * PLOT, svg.ROW - 10, svg.color(lane_name(s["member"] or "")), tip))
        body.append(svg._text(LANE_LABEL + max(s["predicted_ms"] or 0, s["took_ms"]) / peak * PLOT + 6, y + 12, f"{ratio:.2f}x"))
    return svg._svg(len(done) * (svg.ROW + 2) + 4, body)


def shard_states(shards):
    counts = {}
    for s in shards:
        state = "unclaimed" if s.get("member") is None else SHARD_STATES.get(s["state"], str(s["state"]))
        counts[state] = counts.get(state, 0) + 1
    return ", ".join(f"{count} {state}" for state, count in sorted(counts.items()))


def load_chart(load):
    rows = [(node, sum(cpu.get(k, 0) for k in BUSY),
             f"{node}: busy {sum(cpu.get(k, 0) for k in BUSY)} s, idle {cpu.get('idle', 0)} s,"
             f" iowait {cpu.get('iowait', 0)} s, steal {cpu.get('steal', 0)} s, load {cpu.get('load', 0)}")
            for node, cpu in sorted(load.items())]
    return svg.bars(rows, unit=lambda value: f"{value:.0f} s busy")


def statements_table(statements):
    return table(["Statement", "Calls", "Total", "Max"],
                 [[s["query"], s["calls"], f"{s['total_ms']} ms", f"{s['max_ms']} ms"] for s in statements])


def costs_table(costs):
    return table(["Attribute set", "Startup", "Samples", "Children", "Work", "Median child", "Max child"],
                 [[c["path"], f"{c['startup_ms']} ms", c["samples"], c["children"], seconds(c["work_ms"]),
                   f"{c['median_ms']} ms", f"{c['max_ms']} ms"] for c in costs])


def load_spans(out_dir, name):
    path = pathlib.Path(out_dir) / name if out_dir and name else None
    if not path or not path.exists():
        return []
    spans = []
    for line in path.read_text().splitlines():
        if line.startswith("{"):
            raw = json.loads(line)
            spans.append(Span(f"server {raw['target'].split('::')[0]}", 0, raw["name"], raw["ts_us"], raw["dur_us"], raw["fields"]))
    return spans


def span_section(spans):
    if not spans:
        return ""
    totals = {}
    for s in spans:
        total, count, longest = totals.get(s.name, (0, 0, 0))
        totals[s.name] = (total + s.dur_us, count + 1, max(longest, s.dur_us))
    rows = [[name, count, svg.ms(total), svg.ms(longest)]
            for name, (total, count, longest) in sorted(totals.items(), key=lambda item: -item[1][0])[:20]]
    timeline = svg.timeline([s for s in spans if s.dur_us >= TIMELINE_MIN_US])
    return (
        f"<h3>Server Spans</h3><div class=\"card\">{table(['Span', 'Calls', 'Total', 'Max'], rows)}"
        f"<details><summary>Timeline</summary>{timeline}</details>"
        f"<details><summary>Flame graph</summary>{svg.icicle(fold(spans))}</details></div>"
    )


def runs_table(runs):
    head = ["Workers", "Wall clock", "Speedup", "Target", "Statements", "Shards", "Jobs"]
    rows = "".join(
        f"<tr><td>N={n}</td><td>{seconds(r['wall_ms'])}</td><td>{report.speedup(runs, n):.2f}x</td>"
        f"<td>{report.target(n):.2f}x</td><td>{r['statements']}</td><td>{r['shards']}</td><td>{r['jobs']}</td></tr>"
        for n, r in sorted(runs.items())
    )
    return "<table><tr>" + "".join(f"<th>{h}</th>" for h in head) + "</tr>" + rows + "</table>"


def verdict_card(failed):
    if not failed:
        return '<div class="card">All #568 targets met.</div>'
    return '<div class="card"><b>Missed targets</b><pre>' + escape("\n".join(failed)) + "</pre></div>"


def run_section(title, run, out_dir):
    shards = run.get("shard_timings", [])
    aborted = run.get("status", "Completed") != "Completed"
    parts = [
        f"<h2 id=\"{anchor(title)}\">{escape(title)}</h2><p class=\"muted\">{seconds(run['wall_ms'])} on {report.jobs_text(run['jobs'])},"
        f" shards: {shard_states(shards) or 'none'}" + (f", <b>ended {escape(run['status'])} and was aborted</b>" if aborted else "") + "</p>",
        f"<h3>Shards per Member</h3><div class=\"card\">{shard_chart(shards)}</div>",
        f"<h3>Predicted and Actual Shard Time</h3><div class=\"card\">{prediction_chart(shards)}"
        "<span class=\"key\"><i style=\"background:#b8b8c0\"></i>predicted</span>"
        "<span class=\"key\">inner bar: actual, colored by member</span></div>",
        f"<h3>Eval Phases per Job</h3><div class=\"card\">{phase_chart(run.get('phases', []))}"
        f"{svg.legend(['eval_flake', 'eval_derivations'])}</div>",
    ]
    if run.get("load"):
        parts.append(f"<h3>CPU per VM</h3><div class=\"card\">{load_chart(run['load'])}</div>")
    if run.get("top_statements"):
        parts.append(f"<h3>Heaviest Statements</h3><div class=\"card\">{statements_table(run['top_statements'])}</div>")
    if run.get("abort_statements"):
        parts.append(f"<h3>Statements during the Abort</h3><div class=\"card\">{statements_table(run['abort_statements'])}</div>")
    parts.append(span_section(load_spans(out_dir, run.get("spans_file"))))
    if run.get("costs"):
        parts.append(f"<h3>Learned Costs after the Run</h3><div class=\"card\">{costs_table(run['costs'])}</div>")
    return "".join(parts)


def small_section(small):
    if not report.small_complete(small):
        return ""
    rows = [(label, small[label]["wall_ms"] * 1000, f"{label}: {seconds(small[label]['wall_ms'])}"
             f" on {report.jobs_text(small[label]['jobs'])}") for label in ("standalone", "multinode")]
    return f"<h2>Small Flake</h2><div class=\"card\">{svg.bars(rows)}</div>"


def anchor(title):
    return "".join(c if c.isalnum() else "-" for c in title.lower())


def render(runs, small, failed, out_dir=None):
    titled = [(f"N={n}", runs[n]) for n in sorted(runs)]
    if report.small_complete(small):
        titled += [(f"Small Flake, {label}", small[label]) for label in ("standalone", "multinode")]
    nav = "<nav>" + "".join(f'<a href="#{anchor(title)}">{escape(title)}</a>' for title, _ in titled) + "</nav>"
    sections = nav + "".join(run_section(title, run, out_dir) for title, run in titled)
    speedup = (f"<h2>Speedup</h2><div class=\"card\">{speedup_chart(runs)}{runs_table(runs)}</div>"
               if 1 in runs else "")
    return (
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Multinode Evalbench</title>"
        f"<style>{CSS}</style></head><body><h1>Multinode Evalbench</h1>"
        f"{verdict_card(failed)}{speedup}{small_section(small)}{sections}</body></html>"
    )
