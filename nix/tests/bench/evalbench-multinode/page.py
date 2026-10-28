# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

from html import escape

from gradient_evalbench import svg
from gradient_evalbench.report import CSS

import report

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


def run_section(n, run):
    return (
        f"<h2>N={n}</h2><p class=\"muted\">{seconds(run['wall_ms'])} on {report.jobs_text(run['jobs'])},"
        f" {run['shards']} {'shard' if run['shards'] == 1 else 'shards'}</p>"
        f"<h3>Shards per Member</h3><div class=\"card\">{shard_chart(run.get('shard_timings', []))}</div>"
        f"<h3>Eval Phases per Job</h3><div class=\"card\">{phase_chart(run.get('phases', []))}"
        f"{svg.legend(['eval_flake', 'eval_derivations'])}</div>"
    )


def small_section(small):
    if not report.small_complete(small):
        return ""
    rows = [(label, small[label]["wall_ms"] * 1000, f"{label}: {seconds(small[label]['wall_ms'])}"
             f" on {report.jobs_text(small[label]['jobs'])}") for label in ("standalone", "multinode")]
    return f"<h2>Small Flake</h2><div class=\"card\">{svg.bars(rows)}</div>"


def render(runs, small, failed):
    sections = "".join(run_section(n, runs[n]) for n in sorted(runs))
    speedup = (f"<h2>Speedup</h2><div class=\"card\">{speedup_chart(runs)}{runs_table(runs)}</div>"
               if 1 in runs else "")
    return (
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Multinode Evalbench</title>"
        f"<style>{CSS}</style></head><body><h1>Multinode Evalbench</h1>"
        f"{verdict_card(failed)}{speedup}{small_section(small)}{sections}</body></html>"
    )
