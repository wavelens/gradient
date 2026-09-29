# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""Argparse entry point. Stdlib only, so the tool runs wherever python does."""

from __future__ import annotations

import argparse
import pathlib
import sys

from .bundle import NotABundle, open_bundle
from .report import render


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="gradient-evalbench",
        description="Render a Gradient eval benchmark bundle as an HTML report with SVG charts.",
    )
    parser.add_argument("bundle", type=pathlib.Path, help="evalbench.tar.gz or its unpacked directory")
    parser.add_argument("-o", "--output", type=pathlib.Path, default=pathlib.Path("evalbench-report"),
                        help="directory to write index.html and the charts into")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        runs, scratch = open_bundle(args.bundle)
    except (NotABundle, FileNotFoundError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    with scratch:
        index = render(runs, args.output, str(args.bundle))
    print(index)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
