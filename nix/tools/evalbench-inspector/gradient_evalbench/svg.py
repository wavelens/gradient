# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""Hand-written SVG charts. Every shape carries a <title>, so hovering answers
what a bar is without a script."""

from __future__ import annotations

import hashlib
from html import escape

from .bundle import Job, Span
from .flame import Node

WIDTH = 1200
LABEL = 190
ROW = 16
AXIS = 22


def color(name: str) -> str:
    digest = hashlib.sha1(name.encode()).digest()
    return f"hsl({digest[0] * 360 // 256}, {55 + digest[1] % 20}%, {52 + digest[2] % 12}%)"


def ms(us: float) -> str:
    value = us / 1000
    if value >= 10_000:
        return f"{value / 1000:.1f} s"
    if value >= 10:
        return f"{value:.0f} ms"
    return f"{value:.2f} ms"


def _svg(height: int, body: list[str], width: int = WIDTH) -> str:
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" '
        f'width="100%" font-family="ui-monospace, monospace" font-size="11">'
        + "".join(body)
        + "</svg>"
    )


def _rect(x, y, w, h, fill, title, extra="") -> str:
    return (
        f'<rect x="{x:.2f}" y="{y}" width="{max(w, 0.5):.2f}" height="{h}" fill="{fill}"{extra}>'
        f"<title>{escape(title)}</title></rect>"
    )


def _text(x, y, text, anchor="start", cls="label") -> str:
    return f'<text x="{x:.2f}" y="{y}" text-anchor="{anchor}" class="{cls}">{escape(text)}</text>'


def _ticks(span_us: float) -> float:
    magnitude = 1.0
    while True:
        for step in (magnitude, 2 * magnitude, 5 * magnitude):
            if span_us / step <= 12:
                return step
        magnitude *= 10


def _axis(origin_us: float, span_us: float, height: int, plot: float) -> list[str]:
    body = []
    step = _ticks(span_us)
    tick = 0.0
    while tick <= span_us:
        x = LABEL + tick / span_us * plot
        body.append(f'<line x1="{x:.2f}" y1="{AXIS - 4}" x2="{x:.2f}" y2="{height}" class="grid"/>')
        body.append(_text(x, AXIS - 8, ms(tick), "middle", "axis"))
        tick += step
    return body


def bars(rows: list[tuple[str, float, str]], unit=ms) -> str:
    """One horizontal bar per `(label, value, tooltip)`."""
    if not rows:
        return ""
    peak = max(v for _, v, _ in rows) or 1
    plot = WIDTH - LABEL - 90
    body = []
    for i, (label, value, tip) in enumerate(rows):
        y = i * (ROW + 2)
        body.append(_text(LABEL - 6, y + 12, label[:30], "end"))
        body.append(_rect(LABEL, y + 2, value / peak * plot, ROW - 2, color(label), tip or f"{label}: {unit(value)}"))
        body.append(_text(LABEL + value / peak * plot + 6, y + 12, unit(value)))
    return _svg(len(rows) * (ROW + 2) + 4, body)


def timeline(spans: list[Span]) -> str:
    """Every span on its process lane, on the server clock."""
    if not spans:
        return ""
    origin = min(s.ts_us for s in spans)
    span_us = max(s.end_us for s in spans) - origin or 1
    plot = WIDTH - LABEL - 10
    lanes = sorted({(s.process, s.lane) for s in spans})
    row_of = {lane: i for i, lane in enumerate(lanes)}
    height = AXIS + len(lanes) * ROW + 4
    body = _axis(origin, span_us, height, plot)
    previous = None
    for i, (process, lane) in enumerate(lanes):
        if process != previous:
            body.append(_text(LABEL - 6, AXIS + i * ROW + 12, process, "end"))
            previous = process
    for s in sorted(spans, key=lambda s: -s.dur_us):
        x = LABEL + (s.ts_us - origin) / span_us * plot
        args = " ".join(f"{k}={v}" for k, v in s.args.items())
        tip = f"{s.name} {ms(s.dur_us)} @ {ms(s.ts_us - origin)}\n{s.process}\n{args}".strip()
        body.append(_rect(x, AXIS + row_of[(s.process, s.lane)] * ROW + 1, s.dur_us / span_us * plot, ROW - 2, color(s.name), tip))
    return _svg(height, body)


def icicle(root: Node) -> str:
    """Span flame graph: width is time, depth is nesting, top is the process."""
    if not root.total_us:
        return ""
    plot = WIDTH - 2
    body: list[str] = []
    depth = 0

    def draw(node: Node, x: float, level: int):
        nonlocal depth
        width = node.total_us / root.total_us * plot
        if width < 0.3:
            return
        depth = max(depth, level)
        y = level * ROW
        tip = f"{node.name}\ntotal {ms(node.total_us)} in {node.count}\nself {ms(node.self_us)}"
        body.append(_rect(x, y, width, ROW - 1, color(node.name), tip, ' rx="2"'))
        if width > 50:
            body.append(
                f'<text x="{x + 3:.2f}" y="{y + 11}" class="ink">'
                f"{escape(node.name[: int(width / 7)])}</text>"
            )
        offset = x
        for child in sorted(node.children.values(), key=lambda c: -c.total_us):
            draw(child, offset, level + 1)
            offset += child.total_us / root.total_us * plot

    draw(root, 1, 0)
    return _svg((depth + 1) * ROW + 2, body)


def phases(jobs: list[Job]) -> str:
    """Each dispatched job's phases on its own clock, nested phases one row down."""
    rows: list[tuple[str, list]] = []
    for index, job in enumerate(jobs):
        by_seq = {p.seq: p for p in job.phases}

        def level(p) -> int:
            depth = 0
            while p.parent_seq is not None and p.parent_seq in by_seq:
                p = by_seq[p.parent_seq]
                depth += 1
            return depth

        levels: dict[int, list] = {}
        for p in job.phases:
            levels.setdefault(level(p), []).append(p)
        for depth in sorted(levels):
            rows.append((f"{job.kind} #{index}" if depth == 0 else "", levels[depth]))
    if not rows:
        return ""
    span_ms = max(p.end_ms for _, ps in rows for p in ps) or 1
    plot = WIDTH - LABEL - 10
    height = AXIS + len(rows) * ROW + 4
    body = _axis(0, span_ms * 1000, height, plot)
    for i, (label, ps) in enumerate(rows):
        y = AXIS + i * ROW
        if label:
            body.append(_text(LABEL - 6, y + 12, label, "end"))
        for p in ps:
            x = LABEL + p.start_ms / span_ms * plot
            tip = f"{p.name} {ms((p.end_ms - p.start_ms) * 1000)} @ {ms(p.start_ms * 1000)}"
            body.append(_rect(x, y + 1, (p.end_ms - p.start_ms) / span_ms * plot, ROW - 2, color(p.name), tip))
    return _svg(height, body)


def legend(names: list[str]) -> str:
    return "".join(
        f'<span class="key"><i style="background:{color(n)}"></i>{escape(n)}</span>' for n in names
    )
