# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""Reads the `evalbench.tar.gz` bundle (or its unpacked directory) into runs."""

from __future__ import annotations

import json
import pathlib
import tarfile
import tempfile
from dataclasses import dataclass, field

PHASES = {
    0: "fetch",
    1: "push_inputs",
    2: "eval_flake",
    3: "eval_derivations",
    4: "eval_cache_pull",
    5: "eval_cache_push",
    6: "known_derivations_wait",
    7: "drv_closure_push",
    8: "prefetch",
    9: "substitute_relay",
    10: "build",
    11: "compress",
    12: "nar_push",
    13: "cache_query_wait",
    14: "substitute_fetch",
    15: "download",
}

JOB_KINDS = {0: "eval", 1: "build"}


class NotABundle(Exception):
    pass


@dataclass
class Span:
    process: str
    lane: int
    name: str
    ts_us: int
    dur_us: int
    args: dict

    @property
    def end_us(self) -> int:
        return self.ts_us + self.dur_us


@dataclass
class Phase:
    seq: int
    parent_seq: int | None
    name: str
    start_ms: int
    end_ms: int


@dataclass
class Job:
    kind: str
    phases: list[Phase]


@dataclass
class Run:
    name: str
    path: pathlib.Path
    meta: dict
    metrics: dict
    spans: list[Span]
    jobs: list[Job]
    statements: list[dict]
    explain_log: pathlib.Path | None
    flames: dict[str, pathlib.Path] = field(default_factory=dict)

    @property
    def wall_ms(self) -> float:
        if not self.spans:
            return 0.0
        return (max(s.end_us for s in self.spans) - min(s.ts_us for s in self.spans)) / 1000


def _json(path: pathlib.Path, default):
    return json.loads(path.read_text()) if path.exists() else default


def load_spans(trace_json: pathlib.Path) -> list[Span]:
    events = _json(trace_json, {"traceEvents": []})["traceEvents"]
    names = {e["pid"]: e["args"]["name"] for e in events if e.get("ph") == "M" and e.get("name") == "process_name"}
    return [
        Span(
            process=names.get(e["pid"], str(e["pid"])),
            lane=e.get("tid", 0),
            name=e["name"],
            ts_us=e["ts"],
            dur_us=e["dur"],
            args=e.get("args", {}),
        )
        for e in events
        if e.get("ph") == "X"
    ]


def split_jobs(rows: list[dict]) -> list[Job]:
    jobs: list[Job] = []
    for row in rows:
        if row["seq"] == 0 or not jobs:
            jobs.append(Job(kind=JOB_KINDS.get(row["kind"], str(row["kind"])), phases=[]))
        jobs[-1].phases.append(
            Phase(
                seq=row["seq"],
                parent_seq=row["parent_seq"],
                name=PHASES.get(row["phase"], f"phase {row['phase']}"),
                start_ms=row["start_ms"],
                end_ms=row["end_ms"],
            )
        )
    return jobs


def _flames(run_dir: pathlib.Path) -> dict[str, pathlib.Path]:
    candidates = {"server": run_dir / "flame.svg", "worker": run_dir / "worker" / "flame.svg"}
    return {label: path for label, path in candidates.items() if path.exists()}


def load_run(run_dir: pathlib.Path) -> Run:
    metrics = _json(run_dir / "evaluation_metric.json", [])
    explain = run_dir / "pg" / "auto_explain.log"
    return Run(
        name=run_dir.name,
        path=run_dir,
        meta=_json(run_dir / "run.json", {}),
        metrics=metrics[0] if metrics else {},
        spans=load_spans(run_dir / "trace.json"),
        jobs=split_jobs(_json(run_dir / "pg" / "job_phases.json", [])),
        statements=_json(run_dir / "pg" / "pg_stat_statements.json", []),
        explain_log=explain if explain.exists() else None,
        flames=_flames(run_dir),
    )


def _order(run: Run) -> tuple:
    return (bool(run.meta.get("instrumented")), not run.meta.get("cold", False), run.name)


def load_bundle(root: pathlib.Path) -> list[Run]:
    runs = [load_run(p.parent) for p in sorted(root.glob("*/run.json"))]
    if not runs:
        raise NotABundle(f"{root} holds no <run>/run.json")
    return sorted(runs, key=_order)


def unpack(source: pathlib.Path, into: pathlib.Path) -> pathlib.Path:
    if source.is_dir():
        nested = source / "evalbench"
        return nested if nested.is_dir() else source
    if not tarfile.is_tarfile(source):
        raise NotABundle(f"{source} is neither a directory nor a tarball")
    with tarfile.open(source) as tar:
        tar.extractall(into, filter="data")
    nested = into / "evalbench"
    return nested if nested.is_dir() else into


def open_bundle(source: pathlib.Path) -> tuple[list[Run], tempfile.TemporaryDirectory]:
    scratch = tempfile.TemporaryDirectory(prefix="evalbench-")
    return load_bundle(unpack(source, pathlib.Path(scratch.name))), scratch
