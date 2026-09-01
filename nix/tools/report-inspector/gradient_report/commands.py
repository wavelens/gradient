# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""One function per subcommand. Each is a query plus plain-text formatting."""

from __future__ import annotations

import sqlite3

from .db import table_exists

EVALUATION_STATUS = {
    0: "Queued",
    1: "EvaluatingFlake",
    2: "EvaluatingDerivation",
    3: "Building",
    4: "Waiting",
    5: "Completed",
    6: "Failed",
    7: "Aborted",
    8: "Fetching",
}

BUILD_STATUS = {
    0: "Created",
    1: "Queued",
    2: "Building",
    3: "Completed",
    4: "FailedPermanent",
    5: "Aborted",
    6: "DependencyFailed",
    7: "Substituted",
    8: "FailedTransient",
    9: "FailedTimeout",
}

ATTEMPT_REASON = {
    0: "SubstituteUnavailable",
    1: "Oom",
    2: "DiskFull",
    3: "Network",
    4: "BuilderCrash",
    5: "BuilderNonzero",
    6: "WallClockTimeout",
    7: "SilentTimeout",
    8: "InputsUnavailable",
}

# derivation_build.status values that are not a terminal outcome, so an
# evaluation sitting on one of these is waiting rather than finished.
NON_TERMINAL_BUILD_STATUS = (0, 1, 2, 8)

GATES = ("edges_complete", "closure_complete", "drv_closure_cached")


def _lines(rows: list[str]) -> str:
    return "\n".join(rows)


def summary(conn: sqlite3.Connection) -> str:
    meta = conn.execute("SELECT * FROM report_meta").fetchone()
    ev = conn.execute("SELECT * FROM evaluation").fetchone()

    out = [
        f"report schema {meta['schema_version']} from Gradient {meta['gradient_version']}",
        f"generated {meta['generated_at']}",
        "",
    ]

    if ev is None:
        out.append("no evaluation row in this report")
        return _lines(out)

    status = EVALUATION_STATUS.get(ev["status"], str(ev["status"]))
    out += [
        f"evaluation {ev['id']}  status {status}",
        f"  created  {ev['created_at']}",
        f"  fetch    {ev['fetch_started_at']}",
        f"  flake    {ev['eval_flake_started_at']}",
        f"  drv      {ev['eval_drv_started_at']}",
        f"  building {ev['building_started_at']}",
        f"  finished {ev['finished_at']}",
    ]

    # A terminal status with no finish time is a real anomaly, not a display
    # quirk: something ended the evaluation without settling it.
    if status in ("Completed", "Failed", "Aborted") and not ev["finished_at"]:
        out.append("  ! terminal status with no finished_at")

    counts = conn.execute(
        "SELECT status, count(*) AS n FROM derivation_build GROUP BY status ORDER BY n DESC"
    ).fetchall()
    if counts:
        out += ["", "builds by status:"]
        out += [
            f"  {BUILD_STATUS.get(r['status'], r['status']):<18} {r['n']}" for r in counts
        ]

    reasons = conn.execute(
        "SELECT reason, count(*) AS n FROM build_attempt WHERE reason IS NOT NULL "
        "GROUP BY reason ORDER BY n DESC"
    ).fetchall()
    if reasons:
        out += ["", "failure reasons:"]
        out += [
            f"  {ATTEMPT_REASON.get(r['reason'], r['reason']):<22} {r['n']}" for r in reasons
        ]

    out += ["", manifest(conn)]
    return _lines(out)


def manifest(conn: sqlite3.Connection) -> str:
    rows = conn.execute(
        'SELECT "table", rows_included, rows_available, filter FROM report_manifest '
        'ORDER BY "table"'
    ).fetchall()
    out = ["contents:"]
    for r in rows:
        note = "" if r["rows_included"] == r["rows_available"] else f"  ({r['filter']})"
        out.append(
            f"  {r['table']:<24} {r['rows_included']:>6} of {r['rows_available']}{note}"
        )
    return _lines(out)


def timeline(conn: sqlite3.Connection) -> str:
    events: list[tuple[str, str]] = []

    for r in conn.execute("SELECT at, phase, event, worker_id FROM phase_event"):
        events.append((r["at"] or "", f"phase {r['phase']} event {r['event']} worker {r['worker_id']}"))

    for r in conn.execute(
        "SELECT queued_at, dispatched_at, finished_at, worker_id FROM dispatched_job"
    ):
        for label, at in (
            ("queued", r["queued_at"]),
            ("dispatched", r["dispatched_at"]),
            ("finished", r["finished_at"]),
        ):
            if at:
                events.append((at, f"job {label} on {r['worker_id']}"))

    for r in conn.execute(
        "SELECT build_started_at, build_finished_at, outcome, reason FROM build_attempt"
    ):
        if r["build_started_at"]:
            events.append((r["build_started_at"], "attempt started"))
        if r["build_finished_at"]:
            reason = ATTEMPT_REASON.get(r["reason"], "")
            events.append(
                (r["build_finished_at"], f"attempt finished outcome {r['outcome']} {reason}".strip())
            )

    events.sort(key=lambda e: e[0])
    return _lines([f"{at}  {what}" for at, what in events]) or "no timed events"


def why_stuck(conn: sqlite3.Connection) -> str:
    """For each anchor that never reached a terminal state, name the gate that
    is false and the dependency holding it there."""
    placeholders = ", ".join("?" for _ in NON_TERMINAL_BUILD_STATUS)
    anchors = conn.execute(
        f"SELECT id, derivation, status, {', '.join(GATES)} FROM derivation_build "
        f"WHERE status IN ({placeholders})",
        NON_TERMINAL_BUILD_STATUS,
    ).fetchall()

    if not anchors:
        return "no anchor is waiting: every build reached a terminal state"

    out = []
    for a in anchors:
        blocked = [g for g in GATES if not a[g]]
        name = conn.execute(
            "SELECT name FROM derivation WHERE id = ?", (a["derivation"],)
        ).fetchone()
        label = name["name"] if name else a["derivation"]

        if blocked:
            out.append(f"{label}: {BUILD_STATUS.get(a['status'], a['status'])}, waiting on {', '.join(blocked)}")
        else:
            out.append(f"{label}: {BUILD_STATUS.get(a['status'], a['status'])}, every gate open")

        for dep in conn.execute(
            "SELECT d.name, b.status FROM derivation_dependency dd "
            "JOIN derivation d ON d.id = dd.dependency "
            "LEFT JOIN derivation_build b ON b.derivation = dd.dependency "
            "WHERE dd.derivation = ? AND (b.status IS NULL OR b.status NOT IN (3, 7))",
            (a["derivation"],),
        ):
            out.append(f"    dep {dep['name']} status {BUILD_STATUS.get(dep['status'], dep['status'])}")

    return _lines(out)


def failed(conn: sqlite3.Connection, log_for: str | None = None) -> str:
    if log_for:
        if not table_exists(conn, "build_log"):
            return "this report was generated without logs"
        row = conn.execute(
            "SELECT log FROM build_log WHERE build_attempt = ?", (log_for,)
        ).fetchone()
        return row["log"] if row else f"no log for attempt {log_for}"

    rows = conn.execute(
        "SELECT id, outcome, reason, failure_message FROM build_attempt "
        "WHERE outcome IN (3, 4) ORDER BY build_finished_at"
    ).fetchall()
    if not rows:
        return "no failed attempts"

    has_logs = table_exists(conn, "build_log")
    out = []
    for r in rows:
        reason = ATTEMPT_REASON.get(r["reason"], r["reason"])
        out.append(f"{r['id']}  {reason}")
        if r["failure_message"]:
            out.append(f"    {r['failure_message'].splitlines()[0][:160]}")
        if has_logs:
            out.append(f"    log: gradient-report failed --log {r['id']}")
    return _lines(out)


def workers(conn: sqlite3.Connection) -> str:
    if not table_exists(conn, "worker_registration"):
        return "this report was generated without instance context"

    out = ["registrations:"]
    for r in conn.execute(
        "SELECT worker_id, display_name, active, managed, enable_build, created_at "
        "FROM worker_registration"
    ):
        state = "active" if r["active"] else "INACTIVE"
        managed = "managed" if r["managed"] else "self-registered"
        out.append(
            f"  {r['display_name']} ({r['worker_id']}) {state}, {managed}, since {r['created_at']}"
        )

    out += ["", "connections:"]
    connections = 0
    for r in conn.execute(
        "SELECT worker_id, connected_at, disconnected_at, reason FROM worker_connection "
        "ORDER BY connected_at DESC"
    ):
        connections += 1
        if r["disconnected_at"]:
            out.append(
                f"  {r['worker_id']} {r['connected_at']} -> {r['disconnected_at']} reason {r['reason']}"
            )
        else:
            out.append(f"  {r['worker_id']} {r['connected_at']} -> still open")

    # A registered worker that never connected is the whole "workers are not
    # registering" symptom, so say it rather than printing an empty heading.
    if connections == 0:
        out.append("  none: no worker ever connected for this project")

    return _lines(out)


def sql(conn: sqlite3.Connection, query: str) -> str:
    rows = conn.execute(query).fetchall()
    if not rows:
        return "(no rows)"
    header = " | ".join(rows[0].keys())
    body = [" | ".join("" if v is None else str(v) for v in r) for r in rows]
    return _lines([header, "-" * len(header), *body])
