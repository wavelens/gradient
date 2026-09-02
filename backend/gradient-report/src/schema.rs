/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The report file's own tables: what it is, and what it left out.

use std::path::Path;

use anyhow::{Context as _, Result};
use rusqlite::Connection;

/// Bumped whenever an exported table's shape changes, so an inspector can
/// refuse a file it does not understand rather than print wrong answers.
pub const SCHEMA_VERSION: i64 = 2;

#[derive(Clone, Copy, Debug)]
pub struct ReportOptions {
    pub anonymize_identities: bool,
    pub anonymize_packages: bool,
    pub include_logs: bool,
    pub include_instance: bool,
}

pub struct ManifestRow {
    pub table: String,
    pub rows_included: i64,
    pub rows_available: i64,
    /// What the table's `$1` selected. Not always the evaluation: several tables
    /// hang off anchors it shares with other evaluations.
    pub scope: String,
    /// What was dropped from that scope, or `none`.
    pub filter: String,
    pub redactions: String,
}

pub fn open_report(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).context("create report database")?;
    conn.execute_batch(
        r#"
        PRAGMA journal_mode = OFF;
        PRAGMA synchronous = OFF;
        CREATE TABLE report_meta (
            schema_version INTEGER NOT NULL,
            gradient_version TEXT NOT NULL,
            generated_at TEXT NOT NULL,
            evaluation TEXT NOT NULL,
            anonymize_identities INTEGER NOT NULL,
            anonymize_packages INTEGER NOT NULL,
            include_logs INTEGER NOT NULL,
            include_instance INTEGER NOT NULL
        );
        CREATE TABLE report_manifest (
            "table" TEXT NOT NULL PRIMARY KEY,
            rows_included INTEGER NOT NULL,
            rows_available INTEGER NOT NULL,
            scope TEXT NOT NULL,
            filter TEXT NOT NULL,
            redactions TEXT NOT NULL
        );
        "#,
    )
    .context("create report meta tables")?;
    Ok(conn)
}

pub fn write_meta(conn: &Connection, evaluation: &str, opts: &ReportOptions) -> Result<()> {
    conn.execute(
        "INSERT INTO report_meta VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            SCHEMA_VERSION,
            env!("CARGO_PKG_VERSION"),
            gradient_types::now().to_string(),
            evaluation,
            opts.anonymize_identities as i64,
            opts.anonymize_packages as i64,
            opts.include_logs as i64,
            opts.include_instance as i64,
        ],
    )
    .context("write report_meta")?;
    Ok(())
}

pub fn write_manifest(conn: &Connection, rows: &[ManifestRow]) -> Result<()> {
    for row in rows {
        conn.execute(
            "INSERT OR REPLACE INTO report_manifest VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                row.table,
                row.rows_included,
                row.rows_available,
                row.scope,
                row.filter,
                row.redactions
            ],
        )
        .context("write report_manifest")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> ReportOptions {
        ReportOptions {
            anonymize_identities: true,
            anonymize_packages: false,
            include_logs: true,
            include_instance: true,
        }
    }

    #[test]
    fn meta_records_version_and_the_options_used() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = open_report(&dir.path().join("r.db")).expect("open");
        write_meta(&conn, "01a05a38-3276-7252-bc05-c139d9c8a015", &opts()).expect("meta");

        let version: i64 = conn
            .query_row("SELECT schema_version FROM report_meta", [], |r| r.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);

        let packages: i64 = conn
            .query_row("SELECT anonymize_packages FROM report_meta", [], |r| {
                r.get(0)
            })
            .expect("flag");
        assert_eq!(packages, 0, "the flags stored must be the ones asked for");
    }

    /// A filtered report must never read as an empty one: every exported table
    /// says how many rows it had and how many it kept.
    #[test]
    fn manifest_distinguishes_filtered_from_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = open_report(&dir.path().join("r.db")).expect("open");
        write_manifest(
            &conn,
            &[ManifestRow {
                table: "build_log".into(),
                rows_included: 3,
                rows_available: 8805,
                scope: "the evaluation's build anchors".into(),
                filter: "failed attempts only".into(),
                redactions: "none".into(),
            }],
        )
        .expect("manifest");

        let (kept, available, filter): (i64, i64, String) = conn
            .query_row(
                "SELECT rows_included, rows_available, filter FROM report_manifest WHERE \"table\" = 'build_log'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("row");
        assert_eq!((kept, available), (3, 8805));
        assert!(filter.contains("failed"));
    }
}
