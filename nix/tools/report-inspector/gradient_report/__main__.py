# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""Argparse dispatch. Stdlib only, so the tool runs wherever python does."""

from __future__ import annotations

import argparse
import sys

from . import commands, store_spec
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

    spec = sub.add_parser(
        "store-spec", help="write a gradient-daemon store-spec.nix replaying this evaluation"
    )
    spec.add_argument("-o", "--output", required=True, help="file to write")
    spec.add_argument("--name", default="replay", help="spec name, also isolates its derivations")
    spec.add_argument("--time-scale", type=float, default=0.01, help="factor on recorded build durations")
    spec.add_argument("--max-size", type=int, default=65536, help="cap on each output's size in bytes")
    spec.add_argument(
        "--random-durations", action="store_true", help="drop recorded durations, draw them from the timing"
    )
    spec.add_argument(
        "--workers", default="", help="comma-separated workers the fixture runs on; they start with the cached paths"
    )

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
        case "store-spec":
            try:
                spec = store_spec.from_report(
                    conn,
                    name=args.name,
                    time_scale=args.time_scale,
                    max_size=args.max_size,
                    random_durations=args.random_durations,
                    workers=[w for w in args.workers.split(",") if w],
                )
            except store_spec.NotCompleted as e:
                print(f"error: {e}", file=sys.stderr)
                return 2
            with open(args.output, "w", encoding="utf-8") as out:
                out.write(store_spec.render_nix(spec))
        case _:
            print(commands.summary(conn))

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
