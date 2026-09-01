/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Fetching rows and writing them are separate on purpose: `redact_row` and
//! `write_rows` take rows directly, so what a report ends up containing is
//! testable without a database.

use anyhow::{Context as _, Result};
use rusqlite::Connection;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

use crate::redact::Redactor;
use crate::schema::ManifestRow;
use crate::tables::{Row, TableSpec, redact_value};

pub fn redact_row(spec: &TableSpec, redactor: &Redactor, row: &Row) -> Row {
    spec.columns
        .iter()
        .enumerate()
        .map(|(i, column)| redact_value(redactor, spec.name, column, row.get(i).cloned().flatten()))
        .collect()
}

pub fn create_table(conn: &Connection, spec: &TableSpec) -> Result<()> {
    conn.execute(spec.ddl, [])
        .with_context(|| format!("create report table {}", spec.name))?;
    Ok(())
}

pub fn write_rows(conn: &Connection, spec: &TableSpec, rows: &[Row]) -> Result<()> {
    let placeholders = (1..=spec.columns.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({placeholders})",
        spec.name,
        spec.columns.join(", ")
    );

    let mut stmt = conn
        .prepare(&sql)
        .with_context(|| format!("prepare insert for {}", spec.name))?;
    for row in rows {
        stmt.execute(rusqlite::params_from_iter(row.iter()))
            .with_context(|| format!("insert into {}", spec.name))?;
    }

    Ok(())
}

/// Run one spec's query and hand back its rows as text. The only part that
/// needs a database.
pub async fn fetch_rows<C: ConnectionTrait>(
    db: &C,
    spec: &TableSpec,
    scope: &str,
) -> Result<Vec<Row>> {
    let results = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            spec.sql,
            [sea_orm::Value::from(scope)],
        ))
        .await
        .with_context(|| format!("query rows for {}", spec.name))?;

    let mut rows = Vec::with_capacity(results.len());
    for result in &results {
        let mut row = Row::with_capacity(spec.columns.len());
        for i in 0..spec.columns.len() {
            row.push(result.try_get_by_index::<Option<String>>(i).ok().flatten());
        }
        rows.push(row);
    }

    Ok(rows)
}

pub async fn export_tables<C: ConnectionTrait>(
    db: &C,
    conn: &Connection,
    specs: &[TableSpec],
    scope: &str,
    redactor: &Redactor,
) -> Result<Vec<ManifestRow>> {
    let mut manifest = Vec::with_capacity(specs.len());
    for spec in specs {
        create_table(conn, spec)?;
        let rows = fetch_rows(db, spec, scope).await?;
        let redacted: Vec<Row> = rows.iter().map(|r| redact_row(spec, redactor, r)).collect();
        write_rows(conn, spec, &redacted)?;

        manifest.push(ManifestRow {
            table: spec.name.to_owned(),
            rows_included: redacted.len() as i64,
            rows_available: redacted.len() as i64,
            filter: "scoped to the evaluation".to_owned(),
            redactions: redaction_summary(spec, redactor),
        });
    }

    Ok(manifest)
}

/// Which of a table's columns the current options actually rewrote, so the
/// manifest says what happened rather than what was requested.
fn redaction_summary(spec: &TableSpec, redactor: &Redactor) -> String {
    let probe = "gradient-report-probe";
    let changed: Vec<&str> = spec
        .columns
        .iter()
        .copied()
        .filter(|c| {
            redact_value(redactor, spec.name, c, Some(probe.to_owned())).is_some_and(|v| v != probe)
        })
        .collect();

    if changed.is_empty() {
        "none".to_owned()
    } else {
        changed.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ReportOptions, open_report};
    use crate::tables::eval_scope_tables;

    fn spec(name: &str) -> &'static TableSpec {
        eval_scope_tables()
            .iter()
            .find(|s| s.name == name)
            .expect("spec exists")
    }

    fn redactor(identities: bool, packages: bool) -> Redactor {
        Redactor::new(ReportOptions {
            anonymize_identities: identities,
            anonymize_packages: packages,
            include_logs: true,
            include_instance: true,
        })
    }

    fn evaluation_row(repo: &str) -> Row {
        let mut row = vec![None; spec("evaluation").columns.len()];
        row[0] = Some("01a05a38-3276-7252-bc05-c139d9c8a015".into());
        row[2] = Some(repo.into());
        row[5] = Some("7".into());
        row
    }

    #[test]
    fn a_written_row_survives_the_round_trip_with_its_types() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = open_report(&dir.path().join("r.db")).expect("open");
        let spec = spec("evaluation");
        create_table(&conn, spec).expect("ddl");
        write_rows(
            &conn,
            spec,
            &[evaluation_row("https://example.invalid/r.git")],
        )
        .expect("write");

        // Postgres hands every column over as text; SQLite affinity is what
        // turns the numeric ones back into numbers.
        let status: i64 = conn
            .query_row("SELECT status FROM evaluation", [], |r| r.get(0))
            .expect("status");
        assert_eq!(status, 7);
    }

    #[test]
    fn redact_row_rewrites_only_the_columns_with_a_policy() {
        let spec = spec("evaluation");
        let row = evaluation_row("git@git.supersandro.de:sandro/nixos-config.git");
        let out = redact_row(spec, &redactor(true, false), &row);

        assert_eq!(out[0], row[0], "the id is not identifying");
        assert_eq!(out[5], row[5], "the status is not identifying");
        assert_ne!(out[2], row[2], "the repository is");
        assert!(
            !out[2]
                .as_deref()
                .unwrap_or_default()
                .contains("supersandro")
        );
    }

    #[test]
    fn the_manifest_reports_what_was_rewritten_not_what_was_asked() {
        let spec = spec("derivation");
        assert_eq!(redaction_summary(spec, &redactor(true, false)), "none");
        assert_eq!(
            redaction_summary(spec, &redactor(false, true)),
            "name, pname"
        );
    }
}
