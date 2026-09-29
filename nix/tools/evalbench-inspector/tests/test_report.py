# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""The bundle is built here rather than committed: its shape is what
`nix/tests/bench/evalbench/test.py` writes, readable in the diff."""

from __future__ import annotations

import json
import tarfile

from gradient_evalbench import explain
from gradient_evalbench.__main__ import main
from gradient_evalbench.bundle import load_bundle, split_jobs
from gradient_evalbench.flame import fold

TRACE = {
    "traceEvents": [
        {"ph": "M", "name": "process_name", "pid": 1, "args": {"name": "worker (7)"}},
        {"ph": "X", "name": "job", "ts": 0, "dur": 1000, "pid": 1, "tid": 0, "args": {}},
        {"ph": "X", "name": "fetch", "ts": 0, "dur": 600, "pid": 1, "tid": 0, "args": {}},
        {"ph": "X", "name": "eval", "ts": 600, "dur": 300, "pid": 1, "tid": 0, "args": {}},
        {"ph": "X", "name": "wave", "ts": 650, "dur": 100, "pid": 1, "tid": 1, "args": {}},
    ]
}

EXPLAIN = """\
Sep 29 18:47:44 server postgres[10]: [10] LOG:  duration: 5.000 ms  plan:
Sep 29 18:47:44 server postgres[11]: [11] LOG:  duration: 90.500 ms  plan:
Sep 29 18:47:44 server postgres[10]:         Query Text: SELECT 1
Sep 29 18:47:44 server postgres[11]:         Query Text: SELECT slow
Sep 29 18:47:44 server postgres[11]:         Seq Scan on build  (actual rows=9.00 loops=1)
Sep 29 18:47:45 server postgres[10]: [10] LOG:  duration: 7.000 ms  plan:
Sep 29 18:47:45 server postgres[10]:         Query Text: SELECT 1
"""


def build_bundle(root, *, instrumented=False):
    run = root / "evalbench" / "cold-clean"
    (run / "pg").mkdir(parents=True)
    (run / "run.json").write_text(json.dumps({"cold": True, "instrumented": instrumented, "evaluated_s": 1.0}))
    (run / "trace.json").write_text(json.dumps(TRACE))
    (run / "evaluation_metric.json").write_text(json.dumps([{"fetch_ms": 600, "total_thunks": 12}]))
    (run / "pg" / "job_phases.json").write_text(json.dumps([
        {"kind": 0, "seq": 0, "parent_seq": None, "phase": 0, "start_ms": 0, "end_ms": 600},
        {"kind": 0, "seq": 1, "parent_seq": 0, "phase": 12, "start_ms": 100, "end_ms": 500},
        {"kind": 1, "seq": 0, "parent_seq": None, "phase": 10, "start_ms": 0, "end_ms": 50},
    ]))
    (run / "pg" / "pg_stat_statements.json").write_text(json.dumps([
        {"calls": "3", "total_ms": "12.5", "mean_ms": "4.1", "rows": "3", "shared_blks_hit": "9",
         "shared_blks_read": "0", "query": "SELECT <script>"},
    ]))
    if instrumented:
        (run / "pg" / "auto_explain.log").write_text(EXPLAIN)
        (run / "flame.svg").write_text("<svg/>")
    return root / "evalbench"


def test_phase_rows_split_into_jobs_at_each_sequence_restart(tmp_path):
    jobs = load_bundle(build_bundle(tmp_path))[0].jobs
    assert [(j.kind, [p.name for p in j.phases]) for j in jobs] == [
        ("eval", ["fetch", "nar_push"]),
        ("build", ["build"]),
    ]


def test_unknown_phase_codes_stay_visible():
    assert split_jobs([{"kind": 0, "seq": 0, "parent_seq": None, "phase": 99, "start_ms": 0, "end_ms": 1}])[0].phases[0].name == "phase 99"


def test_spans_nest_by_containment_in_their_lane(tmp_path):
    worker = fold(load_bundle(build_bundle(tmp_path))[0].spans).children["worker"]
    job = worker.children["job"]
    assert set(job.children) == {"fetch", "eval"}
    assert job.self_us == 100
    assert worker.children["wave"].total_us == 100, "another lane is its own root"


def test_explain_ranks_plans_across_interleaved_backends(tmp_path):
    log = tmp_path / "auto_explain.log"
    log.write_text(EXPLAIN)
    result = explain.parse(log)
    assert result.plans == 3
    assert [p.duration_ms for p in result.slowest] == [90.5, 7.0, 5.0]
    assert "Seq Scan on build" in result.slowest[0].text
    assert [(t.query, t.count, t.total_ms) for t in result.by_query] == [("SELECT slow", 1, 90.5), ("SELECT 1", 2, 12.0)]


def test_a_tarball_renders_to_an_escaped_page_with_standalone_charts(tmp_path):
    build_bundle(tmp_path / "src", instrumented=True)
    bundle = tmp_path / "evalbench.tar.gz"
    with tarfile.open(bundle, "w:gz") as tar:
        tar.add(tmp_path / "src" / "evalbench", arcname="evalbench")

    assert main([str(bundle), "-o", str(tmp_path / "out")]) == 0

    page = (tmp_path / "out" / "index.html").read_text()
    assert "SELECT &lt;script&gt;" in page and "<script>" not in page
    assert "SELECT slow" in page
    for chart in ("timeline", "span-flame", "phases", "perf-server"):
        assert (tmp_path / "out" / "cold-clean" / f"{chart}.svg").exists()


def test_a_directory_without_runs_is_refused(tmp_path, capsys):
    assert main([str(tmp_path), "-o", str(tmp_path / "out")]) == 2
    assert "run.json" in capsys.readouterr().err
