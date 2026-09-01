# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""Inspect a Gradient evaluation diagnostic report."""

from .db import NotAReport, UnsupportedSchema, open_report

__all__ = ["NotAReport", "UnsupportedSchema", "open_report"]
