/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Snapshot one evaluation, the instance context that explains it, and its
//! failed-build logs into a SQLite file a maintainer can diagnose from without
//! access to the instance.
//!
//! Fetching from Postgres and redacting-and-writing are deliberately separate:
//! the redaction path takes rows directly, so what the file ends up containing
//! is testable without a database.

mod redact;
mod schema;

pub use redact::Redactor;
pub use schema::{
    ManifestRow, ReportOptions, SCHEMA_VERSION, open_report, write_manifest, write_meta,
};
