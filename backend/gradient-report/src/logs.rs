/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Build logs for the attempts that failed.
//!
//! Stored as plain text rather than a compressed blob: the point of shipping a
//! real SQLite file is that any client can read it, and a decompression step
//! would put the most useful column behind tooling.

use anyhow::{Context as _, Result};
use gradient_entity::ids::BuildAttemptId;
use gradient_storage::LogStorage;
use rusqlite::Connection;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

use crate::schema::ManifestRow;

/// Attempts that did not succeed: `AttemptOutcome::Failed` is 3 and `Aborted`
/// is 4. An abort is included because its partial log is often the only record
/// of what a worker was doing before it went quiet. Running, Built and
/// Substituted are the cases deliberately skipped.
const FAILED_ATTEMPT_SQL: &str = "SELECT a.id::text FROM build_attempt a \
     WHERE a.derivation_build IN (SELECT derivation_build FROM build_job WHERE evaluation = $1) \
       AND a.outcome IN (3, 4)";

const ALL_ATTEMPT_SQL: &str = "SELECT count(*)::text FROM build_attempt a \
     WHERE a.derivation_build IN (SELECT derivation_build FROM build_job WHERE evaluation = $1)";

pub fn create_log_table(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE build_log (build_attempt TEXT PRIMARY KEY, log TEXT NOT NULL)",
        [],
    )
    .context("create build_log")?;
    Ok(())
}

pub fn insert_log(conn: &Connection, attempt: &str, log: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO build_log VALUES (?1, ?2)",
        rusqlite::params![attempt, log],
    )
    .context("insert build_log")?;
    Ok(())
}

/// The logs a report will carry, plus how many attempts existed in total so the
/// manifest can show the failed-only filter. Async half; the write is separate
/// because a rusqlite connection cannot cross an await.
pub struct FetchedLogs {
    pub entries: Vec<(String, String)>,
    pub attempts_available: i64,
}

pub async fn fetch_failed_logs<C: ConnectionTrait>(
    db: &C,
    logs: &dyn LogStorage,
    evaluation: &str,
) -> Result<FetchedLogs> {
    let failed = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            FAILED_ATTEMPT_SQL,
            [sea_orm::Value::from(evaluation)],
        ))
        .await
        .context("query failed attempts")?;

    let mut entries = Vec::with_capacity(failed.len());
    for row in &failed {
        let Some(id) = row.try_get_by_index::<Option<String>>(0).ok().flatten() else {
            continue;
        };
        let Ok(attempt_id) = id.parse::<BuildAttemptId>() else {
            continue;
        };

        match logs.read(attempt_id).await {
            Ok(text) if !text.is_empty() => entries.push((id, text)),
            Ok(_) => {}
            Err(e) => tracing::warn!(attempt = %id, error = %e, "report: log unreadable, skipping"),
        }
    }

    let attempts_available = db
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            ALL_ATTEMPT_SQL,
            [sea_orm::Value::from(evaluation)],
        ))
        .await
        .context("count attempts")?
        .and_then(|r| r.try_get_by_index::<Option<String>>(0).ok().flatten())
        .and_then(|c| c.parse::<i64>().ok())
        .unwrap_or(entries.len() as i64);

    Ok(FetchedLogs {
        entries,
        attempts_available,
    })
}

pub fn write_failed_logs(conn: &Connection, logs: &FetchedLogs) -> Result<ManifestRow> {
    create_log_table(conn)?;
    for (attempt, text) in &logs.entries {
        insert_log(conn, attempt, text)?;
    }

    Ok(ManifestRow {
        table: "build_log".to_owned(),
        rows_included: logs.entries.len() as i64,
        rows_available: logs.attempts_available,
        filter: "failed attempts only".to_owned(),
        redactions: "none".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::open_report;

    /// Logs are plain text so any SQLite client can read one, which is the
    /// whole reason the report is a real database rather than an archive.
    #[test]
    fn log_table_stores_readable_text() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = open_report(&dir.path().join("r.db")).expect("open");
        create_log_table(&conn).expect("ddl");
        insert_log(
            &conn,
            "0199-attempt",
            "error: builder failed with exit code 1",
        )
        .expect("insert");

        let text: String = conn
            .query_row(
                "SELECT log FROM build_log WHERE build_attempt = '0199-attempt'",
                [],
                |r| r.get(0),
            )
            .expect("log");
        assert_eq!(text, "error: builder failed with exit code 1");
    }

    /// Successful attempts are the filtered-out case, so the query has to say
    /// so rather than relying on the caller to remember.
    #[test]
    fn only_failed_attempts_are_selected() {
        assert!(FAILED_ATTEMPT_SQL.contains("outcome IN (3, 4)"));
        assert!(FAILED_ATTEMPT_SQL.contains("$1"), "must be eval-scoped");
        assert!(!FAILED_ATTEMPT_SQL.contains('*'));
    }
}
