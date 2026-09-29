# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""Plans from an `auto_explain` journal: the slowest single plans and the
statements that spent the most time in total."""

from __future__ import annotations

import heapq
import pathlib
import re
from dataclasses import dataclass, field

JOURNAL = re.compile(r"^\S+ +\d+ [\d:]+ \S+ postgres\[(\d+)\]: (.*)$")
START = re.compile(r"^\[\d+\] LOG:  duration: ([\d.]+) ms  plan:$")
QUERY = "Query Text: "


@dataclass
class Plan:
    duration_ms: float
    query: str = ""
    lines: list[str] = field(default_factory=list)

    @property
    def text(self) -> str:
        return "\n".join(self.lines)


@dataclass
class QueryTotal:
    query: str
    count: int
    total_ms: float
    max_ms: float


@dataclass
class Explained:
    plans: int
    slowest: list[Plan]
    by_query: list[QueryTotal]


def _plans(lines):
    open_plans: dict[str, Plan] = {}
    for line in lines:
        match = JOURNAL.match(line.rstrip("\n"))
        if not match:
            continue
        pid, message = match.groups()
        start = START.match(message)
        if start:
            if pid in open_plans:
                yield open_plans.pop(pid)
            open_plans[pid] = Plan(duration_ms=float(start.group(1)))
        elif pid in open_plans and message.startswith(" "):
            plan = open_plans[pid]
            body = message.strip()
            if body.startswith(QUERY) and not plan.query:
                plan.query = body.removeprefix(QUERY)
            plan.lines.append(message[8:] if message.startswith(" " * 8) else message)
        elif pid in open_plans:
            yield open_plans.pop(pid)
    yield from open_plans.values()


def parse(path: pathlib.Path, top: int = 25) -> Explained:
    slowest: list[tuple[float, int, Plan]] = []
    totals: dict[str, QueryTotal] = {}
    count = 0
    with path.open(encoding="utf-8", errors="replace") as log:
        for count, plan in enumerate(_plans(log), start=1):
            entry = (plan.duration_ms, count, plan)
            if len(slowest) < top:
                heapq.heappush(slowest, entry)
            else:
                heapq.heappushpop(slowest, entry)
            total = totals.setdefault(plan.query, QueryTotal(plan.query, 0, 0.0, 0.0))
            total.count += 1
            total.total_ms += plan.duration_ms
            total.max_ms = max(total.max_ms, plan.duration_ms)
    return Explained(
        plans=count,
        slowest=[p for _, _, p in sorted(slowest, key=lambda e: -e[0])],
        by_query=sorted(totals.values(), key=lambda t: -t.total_ms)[:top],
    )
