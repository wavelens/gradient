# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""Opening a report, and refusing one this tool does not understand."""

from __future__ import annotations

import sqlite3
from pathlib import Path

# Must track SCHEMA_VERSION in backend/gradient-report/src/schema.rs.
SUPPORTED_SCHEMA = 10


class UnsupportedSchema(Exception):
    """The report was written against a different schema than this inspector reads."""


class NotAReport(Exception):
    """The file opened, but carries no report metadata."""


def open_report(path: str | Path) -> sqlite3.Connection:
    """Open a report read-only, accepting exactly one schema version.

    Refusing anything else matters more than it looks: every command here reads
    columns by name, and the export has both added and dropped columns over its
    life, so a report of any other version does not error at open time - it
    answers from whichever columns still happen to line up, or dies mid-command
    on one that does not. There is no compatibility shim to soften either side.
    """
    conn = sqlite3.connect(f"file:{Path(path)}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row

    try:
        row = conn.execute("SELECT schema_version FROM report_meta").fetchone()
    except sqlite3.DatabaseError as e:
        raise NotAReport(f"{path} is not a Gradient report: {e}") from e

    if row is None:
        raise NotAReport(f"{path} has no report_meta row")

    version = row["schema_version"]
    if version != SUPPORTED_SCHEMA:
        direction = "newer" if version > SUPPORTED_SCHEMA else "older"
        raise UnsupportedSchema(
            f"report schema {version} is {direction} than the one this inspector "
            f"reads ({SUPPORTED_SCHEMA}); use the gradient-report of the report's "
            f"own version"
        )

    return conn


def table_exists(conn: sqlite3.Connection, name: str) -> bool:
    """Whether a table is present. A report generated with logs or instance
    context switched off simply lacks those tables."""
    row = conn.execute(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?", (name,)
    ).fetchone()
    return row is not None
