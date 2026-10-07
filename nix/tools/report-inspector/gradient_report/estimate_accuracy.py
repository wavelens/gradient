# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""How far each element of the recorded EstimatedTimeRule estimate is from the time the job took."""

from __future__ import annotations

import json
import math
import sqlite3
from collections import defaultdict
from dataclasses import dataclass, field
from statistics import median

from .db import table_exists

PREFETCH, BUILD, COMPRESS, SUBSTITUTE_FETCH, DOWNLOAD, NAR_FETCH = 8, 10, 11, 14, 15, 16
EVAL_KIND = 0
COMPLETED = 0

ELEMENTS = ("download", "paths", "build", "substitute", "upload", "eval", "total")
INPUTS = ("paths", "nar_bytes", "output_nar_bytes")
ESTIMATE_FIELD = {
    "download": "download_secs",
    "paths": "path_secs",
    "build": "build_secs",
    "substitute": "build_secs",
    "upload": "upload_secs",
    "eval": "eval_secs",
}
SECS_FIELDS = (
    "download_secs", "path_secs", "build_secs", "oom_retry_secs", "upload_secs", "eval_secs"
)
FEEDS = {
    "missing_nar_size": ("download",),
    "missing_count": ("paths",),
    "build_history": ("build", "substitute", "eval"),
    "core_score": ("build", "substitute", "eval"),
    "download_speed": ("download",),
    "upload_speed": ("upload",),
    "storage_read_speed": ("download",),
    "storage_write_speed": ("upload",),
    "compression_ratio": ("download", "upload"),
    "per_path_secs": ("paths",),
    "output_nar_size": ("upload",),
}

SUBJECTS_SQL = (
    "SELECT a.dispatched_job AS job, group_concat(DISTINCT d.name) AS names "
    "FROM build_attempt a JOIN derivation_build db ON db.id = a.derivation_build "
    "JOIN derivation d ON d.id = db.derivation "
    "WHERE a.dispatched_job IS NOT NULL GROUP BY a.dispatched_job"
)
OOM_KILLS_SQL = (
    "SELECT count(*) AS n FROM derivation_metric m "
    "WHERE m.oom_killed = 1 AND m.worker_id = ? "
    "AND m.created_at BETWEEN ? AND datetime(?, '+60 seconds') "
    "AND m.derivation IN (SELECT db.derivation FROM build_attempt a "
    "JOIN derivation_build db ON db.id = a.derivation_build WHERE a.dispatched_job = ?)"
)

Pair = tuple[float, float]


@dataclass
class Spans:
    secs: dict[int, float] = field(default_factory=lambda: defaultdict(float))
    paths: dict[int, int] = field(default_factory=lambda: defaultdict(int))
    bytes: dict[int, int] = field(default_factory=lambda: defaultdict(int))


@dataclass
class Job:
    subject: str
    worker: str
    fallbacks: list[str]
    elements: dict[str, Pair]
    inputs: dict[str, Pair]
    build: bool
    completed: bool
    oom_chance: float
    oom_kills: int


def estimate_accuracy(conn: sqlite3.Connection, element: str | None = None) -> str:
    jobs, skipped = _load(conn)
    if element is not None:
        return "\n".join(_listing([j for j in jobs if j.completed], element))

    builds = [j for j in jobs if j.build]
    jobs = [j for j in jobs if j.completed]
    out = [f"estimate accuracy over {len(jobs)} completed jobs"]
    if skipped:
        out.append(f"{skipped} jobs recorded before estimates were stored")
    out += ["", *_table("element", ELEMENTS, jobs, lambda j: j.elements, "s")]
    for name in ELEMENTS:
        out += _worst(jobs, name)
    out += ["", *_table("input", INPUTS, jobs, lambda j: j.inputs, "")]
    out += ["", _out_of_memory(builds)]
    out += ["", "fallbacks", *_fallbacks(jobs)]
    return "\n".join(out)


def _load(conn: sqlite3.Connection) -> tuple[list[Job], int]:
    spans = _spans(conn)
    subjects = {r["job"]: r["names"] for r in conn.execute(SUBJECTS_SQL)}
    names = _worker_names(conn)
    jobs: list[Job] = []
    skipped = 0
    for r in conn.execute(
        "SELECT id, kind, outcome, worker_id, dispatched_at, finished_at, score_breakdown, worker_elapsed_ms "
        "FROM dispatched_job WHERE outcome IS NOT NULL"
    ):
        estimate = _estimate(r["score_breakdown"])
        if estimate is None:
            skipped += r["outcome"] == COMPLETED
            continue
        elapsed = None if r["worker_elapsed_ms"] is None else r["worker_elapsed_ms"] / 1000
        build = r["kind"] != EVAL_KIND
        completed = r["outcome"] == COMPLETED
        s = spans.get(r["id"], Spans())
        jobs.append(
            Job(
                subject=subjects.get(r["id"], r["id"]) if build else "evaluation",
                worker=names.get(r["worker_id"], r["worker_id"]),
                fallbacks=estimate.get("fallbacks", []),
                elements=_elements(build, estimate, s, elapsed) if completed else {},
                inputs=_inputs(estimate, s) if build and completed else {},
                build=build,
                completed=completed,
                oom_chance=estimate.get("oom_chance", 0.0),
                oom_kills=_oom_kills(conn, r) if build else 0,
            )
        )
    return jobs, skipped


def _estimate(raw: str | None) -> dict | None:
    try:
        breakdown = json.loads(raw) if raw else None
    except json.JSONDecodeError:
        return None
    return breakdown.get("estimate") if isinstance(breakdown, dict) else None


def _spans(conn: sqlite3.Connection) -> dict[str, Spans]:
    out: dict[str, Spans] = defaultdict(Spans)
    for r in conn.execute(
        "SELECT dispatched_job, phase, end_ms - start_ms AS ms, paths, bytes FROM dispatched_job_phase"
    ):
        s = out[r["dispatched_job"]]
        s.secs[r["phase"]] += (r["ms"] or 0) / 1000
        s.paths[r["phase"]] += r["paths"] or 0
        s.bytes[r["phase"]] += r["bytes"] or 0
    return out


def _worker_names(conn: sqlite3.Connection) -> dict[str, str]:
    names: dict[str, str] = {}
    for table in ("worker_registration", "team_worker"):
        if table_exists(conn, table):
            for r in conn.execute(f"SELECT worker_id, display_name FROM {table} WHERE display_name <> ''"):
                names.setdefault(r["worker_id"], r["display_name"])
    return names


def _total(estimate: dict) -> float:
    return sum(estimate.get(f, 0.0) for f in SECS_FIELDS)


def _compared(pairs: dict[str, Pair]) -> dict[str, Pair]:
    return {name: p for name, p in pairs.items() if p[0] > 0 or p[1] > 0}


def _elements(build: bool, estimate: dict, s: Spans, elapsed: float | None) -> dict[str, Pair]:
    return _build_elements(estimate, s, elapsed) if build else _eval_elements(estimate, elapsed)


def _build_elements(estimate: dict, s: Spans, elapsed: float | None) -> dict[str, Pair]:
    fetch = s.secs.get(NAR_FETCH, 0.0)
    built = s.secs.get(BUILD, 0.0)
    substituted = s.secs.get(SUBSTITUTE_FETCH, 0.0) + s.secs.get(DOWNLOAD, 0.0)
    run = "build" if built > 0 or substituted <= 0 else "substitute"
    actual = {
        "download": fetch,
        "paths": max(s.secs.get(PREFETCH, 0.0) - fetch, 0.0),
        run: built if run == "build" else substituted,
        "upload": s.secs.get(COMPRESS, 0.0),
    }
    pairs = {name: (estimate.get(ESTIMATE_FIELD[name], 0.0), secs) for name, secs in actual.items()}
    if elapsed is not None:
        pairs["total"] = (_total(estimate), elapsed)
    return _compared(pairs)


def _eval_elements(estimate: dict, elapsed: float | None) -> dict[str, Pair]:
    if elapsed is None:
        return {}
    return _compared({"eval": (estimate.get("eval_secs", 0.0), elapsed), "total": (_total(estimate), elapsed)})


def _inputs(estimate: dict, s: Spans) -> dict[str, Pair]:
    return _compared(
        {
            "paths": (estimate.get("paths", 0.0), s.paths.get(PREFETCH, 0)),
            "nar_bytes": (estimate.get("nar_bytes", 0.0), s.bytes.get(PREFETCH, 0)),
            "output_nar_bytes": (estimate.get("output_nar_bytes", 0.0), s.bytes.get(COMPRESS, 0)),
        }
    )


def _oom_kills(conn: sqlite3.Connection, r: sqlite3.Row) -> int:
    return conn.execute(
        OOM_KILLS_SQL, (r["worker_id"], r["dispatched_at"], r["finished_at"], r["id"])
    ).fetchone()["n"]


def _ratios(pairs: list[Pair]) -> list[float]:
    return sorted(actual / estimated for estimated, actual in pairs if estimated > 0)


def _error(pair: Pair) -> float:
    estimated, actual = pair
    if estimated <= 0:
        return math.inf
    return abs(math.log(max(actual, 1e-3) / estimated))


def _quantile(values: list[float], q: float) -> float | None:
    return values[min(len(values) - 1, int(q * len(values)))] if values else None


def _x(ratio: float | None) -> str:
    return "-" if ratio is None else f"x{ratio:.2f}"


def _amount(value: float, unit: str) -> str:
    return f"{value:.1f}s" if unit == "s" else f"{value:.0f}"


def _table(title: str, names, jobs: list[Job], pick, unit: str) -> list[str]:
    rows = [
        f"{title:<17} {'jobs':>5} {'estimated':>12} {'actual':>12} {'median':>7} {'p10':>7} {'p90':>7} {'zero':>5}"
    ]
    for name in names:
        pairs = [pick(j)[name] for j in jobs if name in pick(j)]
        if not pairs:
            continue
        ratios = _ratios(pairs)
        rows.append(
            f"{name:<17} {len(pairs):>5} {_amount(sum(e for e, _ in pairs), unit):>12} "
            f"{_amount(sum(a for _, a in pairs), unit):>12} {_x(median(ratios) if ratios else None):>7} "
            f"{_x(_quantile(ratios, 0.1)):>7} {_x(_quantile(ratios, 0.9)):>7} "
            f"{sum(1 for e, _ in pairs if e <= 0):>5}"
        )
    return rows


def _worst(jobs: list[Job], name: str) -> list[str]:
    measured = [j for j in jobs if name in j.elements and j.elements[name][0] > 0]
    if not measured:
        return []
    measured.sort(key=lambda j: _error(j.elements[name]), reverse=True)
    out = ["", f"worst {name}:"]
    for j in measured[:3]:
        estimated, actual = j.elements[name]
        out.append(
            f"  {j.subject} on {j.worker}: estimated {estimated:.1f}s, took {actual:.1f}s "
            f"({_x(actual / estimated)})"
        )
    return out


def _out_of_memory(jobs: list[Job]) -> str:
    builds = [j for j in jobs if j.build]
    expected = sum(j.oom_chance for j in builds)
    kills = sum(j.oom_kills for j in builds)
    return f"out of memory: expected {expected:.2f} kills over {len(builds)} build jobs, {kills} recorded"


def _fallbacks(jobs: list[Job]) -> list[str]:
    out = []
    for fallback, elements in FEEDS.items():
        using = [j for j in jobs if fallback in j.fallbacks]
        if not using:
            continue
        others = [j for j in jobs if fallback not in j.fallbacks]
        for name in elements:
            with_it = _ratios([j.elements[name] for j in using if name in j.elements])
            if not with_it:
                continue
            without = _ratios([j.elements[name] for j in others if name in j.elements])
            out.append(
                f"  {fallback:<20} {len(using):>4} jobs  {name} {_x(median(with_it))} "
                f"(others {_x(median(without) if without else None)})"
            )
    return out


def _listing(jobs: list[Job], name: str) -> list[str]:
    measured = [j for j in jobs if name in j.elements]
    measured.sort(key=lambda j: _error(j.elements[name]), reverse=True)
    out = []
    for j in measured:
        estimated, actual = j.elements[name]
        ratio = _x(actual / estimated) if estimated > 0 else "zero"
        out.append(
            f"{j.subject}  {j.worker}  estimated {estimated:.1f}s  took {actual:.1f}s  {ratio}  "
            f"{','.join(j.fallbacks)}"
        )
    return out
