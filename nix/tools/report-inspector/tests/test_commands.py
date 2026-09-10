# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""The fixture is built here rather than committed as a binary: a checked-in
.db drifts silently from the schema it is meant to represent, and this way the
expected shape is readable in the diff."""

from __future__ import annotations

import sqlite3

import pytest

from gradient_report import commands
from gradient_report.db import SUPPORTED_SCHEMA, NotAReport, UnsupportedSchema, open_report

EVAL_ID = "01a05a38-3276-7252-bc05-c139d9c8a015"


def build_report(path, *, schema_version: int = SUPPORTED_SCHEMA, with_instance: bool = True) -> None:
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE report_meta (schema_version INTEGER, gradient_version TEXT,
            generated_at TEXT, evaluation TEXT, anonymize_identities INTEGER,
            anonymize_packages INTEGER, include_logs INTEGER, include_instance INTEGER);
        CREATE TABLE report_manifest ("table" TEXT, rows_included INTEGER,
            rows_available INTEGER, filter TEXT, redactions TEXT);
        CREATE TABLE evaluation (id TEXT, status INTEGER, created_at TEXT,
            fetch_started_at TEXT, eval_flake_started_at TEXT, eval_drv_started_at TEXT,
            building_started_at TEXT, finished_at TEXT);
        CREATE TABLE derivation (id TEXT, name TEXT, walked INTEGER);
        CREATE TABLE derivation_build (id TEXT, derivation TEXT, status INTEGER,
            substitutable INTEGER, fetchable INTEGER, unready_deps INTEGER);
        CREATE TABLE derivation_dependency (id TEXT, derivation TEXT, dependency TEXT);
        CREATE TABLE build_attempt (id TEXT, outcome INTEGER, reason INTEGER,
            failure_message TEXT, build_started_at TEXT, build_finished_at TEXT);
        CREATE TABLE dispatched_job (queued_at TEXT, dispatched_at TEXT,
            finished_at TEXT, worker_id TEXT);
        CREATE TABLE phase_event (at TEXT, phase INTEGER, event INTEGER, worker_id TEXT);
        CREATE TABLE build_log (build_attempt TEXT PRIMARY KEY, log TEXT);
        """
    )
    conn.execute(
        "INSERT INTO report_meta VALUES (?, '1.3.0', '2026-09-01T00:00:00', ?, 1, 0, 1, 1)",
        (schema_version, EVAL_ID),
    )
    # The real case this was built for: Aborted, but never settled.
    conn.execute(
        "INSERT INTO evaluation VALUES (?, 7, '2026-08-31T23:47:07', '2026-08-31T23:47:10',"
        " '2026-08-31T23:48:08', '2026-08-31T23:48:08', NULL, NULL)",
        (EVAL_ID,),
    )
    conn.execute("INSERT INTO derivation VALUES ('d1', 'vendor-registry', 0)")
    conn.execute("INSERT INTO derivation VALUES ('d2', 'cargo-package-clap_complete-4.6.9', 1)")
    conn.execute("INSERT INTO derivation VALUES ('d3', 'nixos-system-builder-1', 1)")
    conn.execute("INSERT INTO derivation VALUES ('d4', 'openssl-3.7.2', 1)")
    conn.execute("INSERT INTO derivation VALUES ('d5', 'zlib-1.3.2', 1)")
    # b1 is blocked on both gates the report carries, b3 only on the count, so the
    # two are proven separately and neither is named while it is open. b4 and b5 both
    # have every visible gate open; only b5 is substitutable, so only b5's `.drv`
    # gate is knowable from the report.
    conn.execute("INSERT INTO derivation_build VALUES ('b1', 'd1', 1, 0, 0, 1)")
    conn.execute("INSERT INTO derivation_build VALUES ('b2', 'd2', 4, 0, 0, 0)")
    conn.execute("INSERT INTO derivation_build VALUES ('b3', 'd3', 1, 0, 0, 2)")
    conn.execute("INSERT INTO derivation_build VALUES ('b4', 'd4', 0, 0, 0, 0)")
    conn.execute("INSERT INTO derivation_build VALUES ('b5', 'd5', 1, 1, 1, 0)")
    conn.execute("INSERT INTO derivation_dependency VALUES ('dd1', 'd1', 'd2')")
    conn.execute("INSERT INTO derivation_dependency VALUES ('dd2', 'd3', 'd1')")
    conn.execute("INSERT INTO derivation_dependency VALUES ('dd3', 'd3', 'd2')")
    conn.execute(
        "INSERT INTO build_attempt VALUES ('a1', 3, 8, 'input prefetch failed', "
        "'2026-08-31T23:47:30', '2026-08-31T23:47:50')"
    )
    conn.execute("INSERT INTO build_log VALUES ('a1', 'error: NAR size mismatch')")
    conn.execute(
        "INSERT INTO report_manifest VALUES ('build_log', 1, 8805, 'failed attempts only', 'none')"
    )

    if with_instance:
        conn.executescript(
            """
            CREATE TABLE worker_registration (worker_id TEXT, display_name TEXT,
                active INTEGER, managed INTEGER, enable_build INTEGER, created_at TEXT);
            CREATE TABLE worker_connection (worker_id TEXT, connected_at TEXT,
                disconnected_at TEXT, reason INTEGER);
            CREATE TABLE config_snapshot (key TEXT, value TEXT);
            """
        )
        conn.execute(
            "INSERT INTO worker_registration VALUES ('w1', 'builder-1', 0, 0, 1, '2026-08-01')"
        )
        conn.execute("INSERT INTO config_snapshot VALUES ('inputs_unavailable_max_loops', '3')")

    conn.commit()
    conn.close()


@pytest.fixture
def report(tmp_path):
    path = tmp_path / "r.db"
    build_report(path)
    return open_report(path)


def test_refuses_a_report_newer_than_the_schema_it_reads(tmp_path):
    path = tmp_path / "future.db"
    build_report(path, schema_version=999)
    with pytest.raises(UnsupportedSchema, match="newer"):
        open_report(path)


# A dropped column is as fatal as an added one: schema 6 has no
# `derivation.walked`, so opening it would only defer the crash to a command.
def test_refuses_a_report_older_than_the_schema_it_reads(tmp_path):
    path = tmp_path / "past.db"
    build_report(path, schema_version=SUPPORTED_SCHEMA - 1)
    with pytest.raises(UnsupportedSchema, match="older"):
        open_report(path)


def test_rejects_a_file_that_is_not_a_report(tmp_path):
    path = tmp_path / "random.db"
    sqlite3.connect(path).execute("CREATE TABLE t (x INTEGER)")
    with pytest.raises(NotAReport):
        open_report(path)


def test_summary_flags_a_terminal_status_that_never_settled(report):
    out = commands.summary(report)
    assert "Aborted" in out
    assert "terminal status with no finished_at" in out


def test_summary_counts_builds_and_failure_reasons(report):
    out = commands.summary(report)
    assert "FailedPermanent" in out
    assert "InputsUnavailable" in out


def test_manifest_shows_what_was_filtered_out(report):
    out = commands.manifest(report)
    assert "1 of 8805" in out
    assert "failed attempts only" in out


def test_why_stuck_names_the_gate_and_the_blocking_dependency(report):
    lines = commands.why_stuck(report).splitlines()
    stuck = next(line for line in lines if line.startswith("vendor-registry"))
    counted = next(line for line in lines if line.startswith("nixos-system-builder-1"))

    assert "walked" in stuck
    assert "unready_deps = 1" in stuck
    # `fetchable` says whether this anchor can serve its DEPENDENTS, so it is
    # never a gate on the anchor itself, and `walked` is open on b3.
    assert "fetchable" not in stuck
    assert "unready_deps = 2" in counted
    assert "walked" not in counted

    assert "    dep cargo-package-clap_complete-4.6.9 status FailedPermanent not fetchable" in lines


def test_why_stuck_only_claims_the_gates_the_report_can_see(report):
    lines = commands.why_stuck(report).splitlines()
    non_substitutable = next(
        line for line in lines if line.startswith("openssl-3.7.2")
    )
    substitutable = next(line for line in lines if line.startswith("zlib-1.3.2"))

    # The `build_job` gate is open for every exported anchor by construction, so the
    # only gate the report cannot weigh is a non-substitutable anchor's own `.drv`.
    assert "every gate open" not in non_substitutable
    assert "own .drv is whole" in non_substitutable
    assert "build_job" not in non_substitutable
    assert substitutable.endswith("every gate open")


def test_failed_lists_attempts_and_dumps_one_log(report):
    listing = commands.failed(report)
    assert "InputsUnavailable" in listing
    assert "input prefetch failed" in listing
    assert commands.failed(report, "a1") == "error: NAR size mismatch"
    assert "no log" in commands.failed(report, "nope")


def test_workers_says_so_when_none_ever_connected(report):
    out = commands.workers(report)
    assert "INACTIVE" in out
    assert "no worker ever connected" in out


def test_commands_degrade_when_a_section_was_not_included(tmp_path):
    path = tmp_path / "eval-only.db"
    build_report(path, with_instance=False)
    conn = open_report(path)
    assert "without instance context" in commands.workers(conn)


def test_sql_is_raw_access(report):
    out = commands.sql(report, "SELECT name FROM derivation ORDER BY name")
    assert "vendor-registry" in out
    assert commands.sql(report, "SELECT 1 WHERE 0") == "(no rows)"
