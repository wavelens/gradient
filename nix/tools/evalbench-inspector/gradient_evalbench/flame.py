# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""Folds trace spans into a call tree by containment within their lane, the
same nesting `summarize.py` preserved when it assigned the lanes."""

from __future__ import annotations

from dataclasses import dataclass, field

from .bundle import Span


@dataclass
class Node:
    name: str
    total_us: int = 0
    count: int = 0
    children: dict[str, "Node"] = field(default_factory=dict)

    def child(self, name: str) -> "Node":
        return self.children.setdefault(name, Node(name))

    @property
    def self_us(self) -> int:
        return max(0, self.total_us - sum(c.total_us for c in self.children.values()))


def fold(spans: list[Span]) -> Node:
    root = Node("all")
    lanes: dict[tuple[str, int], list[Span]] = {}
    for span in spans:
        lanes.setdefault((span.process, span.lane), []).append(span)

    for (process, _), own in sorted(lanes.items()):
        top = root.child(process.split(" ")[0])
        stack: list[tuple[int, Node]] = []
        for span in sorted(own, key=lambda s: (s.ts_us, -s.dur_us)):
            while stack and stack[-1][0] < span.end_us:
                stack.pop()
            parent = stack[-1][1] if stack else top
            node = parent.child(span.name)
            node.total_us += span.dur_us
            node.count += 1
            if not stack:
                top.total_us += span.dur_us
                top.count += 1
            stack.append((span.end_us, node))
    root.total_us = sum(c.total_us for c in root.children.values())
    return root
