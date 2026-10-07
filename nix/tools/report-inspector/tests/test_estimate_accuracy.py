# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""j1 and j2 are builds, j3 an evaluation, j4 to j6 must not be compared."""

from __future__ import annotations

import json
import sqlite3

import pytest

from gradient_report.db import SUPPORTED_SCHEMA, open_report
from gradient_report.estimate_accuracy import estimate_accuracy

PREFETCH, BUILD, COMPRESS, NAR_FETCH = 8, 10, 11, 16


def breakdown(**estimate) -> str:
    full = {
        "download_secs": 0.0, "path_secs": 0.0, "build_secs": 0.0, "oom_retry_secs": 0.0,
        "upload_secs": 0.0, "eval_secs": 0.0, "nar_bytes": 0.0, "paths": 0.0,
        "output_nar_bytes": 0.0, "oom_chance": 0.0, "fallbacks": [],
    }
    full.update(estimate)
    return json.dumps({"rules": {}, "total": 0.0, "estimate": full})


@pytest.fixture
def report(tmp_path):
    path = tmp_path / "r.db"
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE report_meta (schema_version INTEGER);
        CREATE TABLE dispatched_job (id TEXT, kind INTEGER, worker_id TEXT, dispatched_at TEXT,
            finished_at TEXT, outcome INTEGER, score_breakdown TEXT, worker_elapsed_ms INTEGER);
        CREATE TABLE dispatched_job_phase (dispatched_job TEXT, phase INTEGER, start_ms INTEGER,
            end_ms INTEGER, paths INTEGER, bytes INTEGER);
        CREATE TABLE build_attempt (id TEXT, dispatched_job TEXT, derivation_build TEXT);
        CREATE TABLE derivation_build (id TEXT, derivation TEXT);
        CREATE TABLE derivation (id TEXT, name TEXT);
        CREATE TABLE derivation_metric (derivation TEXT, worker_id TEXT, oom_killed INTEGER,
            created_at TEXT);
        CREATE TABLE worker_registration (worker_id TEXT, display_name TEXT);
        """
    )
    conn.execute("INSERT INTO report_meta VALUES (?)", (SUPPORTED_SCHEMA,))
    conn.execute("INSERT INTO worker_registration VALUES ('w1', 'builder-1')")
    jobs = [
        ("j1", 1, "w1", 0, breakdown(download_secs=10.0, path_secs=2.0, build_secs=30.0,
            oom_retry_secs=15.0, upload_secs=5.0, nar_bytes=1000.0, paths=4.0,
            output_nar_bytes=500.0, oom_chance=0.5), 80_000),
        ("j2", 1, "w2", 0, breakdown(download_secs=10.0, build_secs=100.0, nar_bytes=1000.0,
            fallbacks=["missing_nar_size"]), None),
        ("j3", 0, "w1", 0, breakdown(eval_secs=20.0), 30_000),
        ("j4", 1, "w1", 0, json.dumps({"rules": {}, "total": 0.0}), 1_000),
        ("j5", 1, "w1", 0, None, 1_000),
        ("j6", 1, "w1", 1, breakdown(build_secs=1.0), 1_000),
    ]
    for job, kind, worker, outcome, score, elapsed in jobs:
        conn.execute(
            "INSERT INTO dispatched_job VALUES (?, ?, ?, '2026-10-08 10:00:00',"
            " '2026-10-08 10:02:30', ?, ?, ?)",
            (job, kind, worker, outcome, score, elapsed),
        )
    phases = [
        ("j1", PREFETCH, 0, 15_000, 4, 1000), ("j1", NAR_FETCH, 0, 10_000, 4, 1000),
        ("j1", BUILD, 15_000, 75_000, 0, 0), ("j1", COMPRESS, 75_000, 80_000, 1, 500),
        ("j2", PREFETCH, 0, 50_000, 0, 4000), ("j2", NAR_FETCH, 0, 40_000, 0, 4000),
        ("j2", BUILD, 50_000, 150_000, 0, 0),
        ("j6", BUILD, 0, 99_000, 0, 0),
    ]
    conn.executemany("INSERT INTO dispatched_job_phase VALUES (?, ?, ?, ?, ?, ?)", phases)
    conn.executemany("INSERT INTO derivation VALUES (?, ?)", [("d1", "openssl-3.7.2"), ("d2", "zlib-1.3.2")])
    conn.executemany("INSERT INTO derivation_build VALUES (?, ?)", [("b1", "d1"), ("b2", "d2")])
    conn.executemany(
        "INSERT INTO build_attempt VALUES (?, ?, ?)", [("a1", "j1", "b1"), ("a2", "j2", "b2")]
    )
    conn.executemany(
        "INSERT INTO derivation_metric VALUES (?, ?, ?, ?)",
        [("d1", "w1", 1, "2026-10-08 10:01:30"), ("d1", "w1", 1, "2026-10-08 11:00:00")],
    )
    conn.commit()
    conn.close()
    return open_report(path)


def row(out: str, name: str) -> list[str]:
    return next(line.split() for line in out.splitlines() if line.split()[:1] == [name])


def test_each_element_is_set_against_its_phases(report):
    out = estimate_accuracy(report)
    assert row(out, "download") == ["download", "2", "20.0s", "50.0s", "x2.50", "x1.00", "x4.00", "0"]
    assert row(out, "build") == ["build", "2", "130.0s", "160.0s", "x1.50", "x1.00", "x2.00", "0"]
    assert row(out, "eval") == ["eval", "1", "20.0s", "30.0s", "x1.50", "x1.50", "x1.50", "0"]


def test_an_element_estimated_zero_that_took_time_counts_apart(report):
    assert row(estimate_accuracy(report), "paths") == [
        "paths", "2", "2.0s", "15.0s", "x2.50", "x2.50", "x2.50", "1"
    ]


def test_an_element_without_estimate_or_time_is_not_compared(report):
    assert row(estimate_accuracy(report), "upload")[1] == "1"


def test_total_needs_the_worker_elapsed_time(report):
    assert row(estimate_accuracy(report), "total")[1] == "2"


def test_jobs_before_estimates_and_unfinished_jobs_are_left_out(report):
    out = estimate_accuracy(report)
    assert "estimate accuracy over 3 completed jobs" in out
    assert "2 jobs recorded before estimates were stored" in out


def test_the_worst_job_comes_first_with_its_worker_name(report):
    out = estimate_accuracy(report).splitlines()
    worst = out[out.index("worst build:") + 1]
    assert worst == "  openssl-3.7.2 on builder-1: estimated 30.0s, took 60.0s (x2.00)"


def test_recorded_inputs_are_compared(report):
    assert row(estimate_accuracy(report), "nar_bytes")[4] == "x2.50"


def test_out_of_memory_kills_are_counted_inside_the_job(report):
    assert "out of memory: expected 0.50 kills over 2 build jobs, 1 recorded" in estimate_accuracy(report)


def test_a_fallback_is_set_against_the_jobs_without_it(report):
    line = next(l for l in estimate_accuracy(report).splitlines() if "missing_nar_size" in l)
    assert line.split() == ["missing_nar_size", "1", "jobs", "download", "x4.00", "(others", "x1.00)"]


def test_an_element_lists_its_jobs_worst_first(report):
    lines = estimate_accuracy(report, "build").splitlines()
    assert lines[0].split()[:2] == ["openssl-3.7.2", "builder-1"]
    assert lines[1].split()[:2] == ["zlib-1.3.2", "w2"]
    assert lines[1].split()[-1] == "missing_nar_size"
