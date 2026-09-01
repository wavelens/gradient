# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""Argparse dispatch. Stdlib only, so the tool runs wherever python does."""

from __future__ import annotations

import argparse
import sys

from . import commands
from .db import NotAReport, UnsupportedSchema, open_report


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="gradient-report",
        description="Inspect a Gradient evaluation diagnostic report.",
    )
    parser.add_argument("report", help="path to the .db file")
    sub = parser.add_subparsers(dest="command")

    sub.add_parser("summary", help="status, timings, build and failure counts (default)")
    sub.add_parser("timeline", help="phase events, dispatches and attempts in order")
    sub.add_parser("why-stuck", help="which gate each waiting anchor is held by")
    sub.add_parser("workers", help="registration and connection history")
    sub.add_parser("manifest", help="what the report contains and what it left out")

    failed = sub.add_parser("failed", help="failed attempts, and one attempt's log")
    failed.add_argument("--log", metavar="ATTEMPT", help="print this attempt's log")

    raw = sub.add_parser("sql", help="run a query against the report")
    raw.add_argument("query")

    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)

    try:
        conn = open_report(args.report)
    except (UnsupportedSchema, NotAReport) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2

    match args.command:
        case "timeline":
            print(commands.timeline(conn))
        case "why-stuck":
            print(commands.why_stuck(conn))
        case "workers":
            print(commands.workers(conn))
        case "manifest":
            print(commands.manifest(conn))
        case "failed":
            print(commands.failed(conn, args.log))
        case "sql":
            print(commands.sql(conn, args.query))
        case _:
            print(commands.summary(conn))

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
