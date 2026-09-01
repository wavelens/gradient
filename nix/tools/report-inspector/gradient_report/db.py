# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""Opening a report, and refusing one this tool does not understand."""

from __future__ import annotations

import sqlite3
from pathlib import Path

# Must track SCHEMA_VERSION in backend/gradient-report/src/schema.rs.
SUPPORTED_SCHEMA = 1


class UnsupportedSchema(Exception):
    """The report was written by a Gradient newer than this inspector."""


class NotAReport(Exception):
    """The file opened, but carries no report metadata."""


def open_report(path: str | Path) -> sqlite3.Connection:
    """Open a report read-only, checking its schema version first.

    Refusing an unknown version matters more than it looks: every command here
    reads columns by name, so a newer report would not error, it would quietly
    answer from the columns that happen to still match.
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
    if version > SUPPORTED_SCHEMA:
        raise UnsupportedSchema(
            f"report schema {version} is newer than this inspector understands "
            f"({SUPPORTED_SCHEMA}); upgrade gradient-report"
        )

    return conn


def table_exists(conn: sqlite3.Connection, name: str) -> bool:
    """Whether a table is present. A report generated with logs or instance
    context switched off simply lacks those tables."""
    row = conn.execute(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?", (name,)
    ).fetchone()
    return row is not None
