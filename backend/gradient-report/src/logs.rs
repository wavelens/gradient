/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Logs are stored as plain text rather than a compressed blob. Any SQLite client can then read the
//! most useful column without extra tooling.

use anyhow::{Context as _, Result};
use gradient_entity::ids::BuildAttemptId;
use gradient_storage::LogStorage;
use rusqlite::Connection;
use sea_orm::ConnectionTrait;
use sea_orm::prelude::Uuid;

use crate::redact::Redactor;
use crate::schema::ManifestRow;

gradient_db::sql! {
    /// `AttemptOutcome::Failed` is 3 and `Aborted` is 4. An abort's partial log is often the only
    /// record of what a worker was doing before it went quiet.
    FAILED_ATTEMPT_SQL = "SELECT a.id::text FROM build_attempt a \
     WHERE a.derivation_build IN (SELECT derivation_build FROM build_job WHERE evaluation = $1) \
       AND a.outcome IN (3, 4)",
        params = [EvaluationId],
        tier = Bulk,
        budget = gradient_db::sql::Budget::bulk().seq_scan_allowed();

    ALL_ATTEMPT_SQL = "SELECT count(*)::text FROM build_attempt a \
     WHERE a.derivation_build IN (SELECT derivation_build FROM build_job WHERE evaluation = $1)",
        params = [EvaluationId],
        tier = Bulk;
}

pub fn create_log_table(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE build_log (build_attempt TEXT PRIMARY KEY, log TEXT NOT NULL)",
        [],
    )
    .context("create build_log")?;
    Ok(())
}

pub fn insert_log(conn: &Connection, redactor: &Redactor, attempt: &str, log: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO build_log VALUES (?1, ?2)",
        rusqlite::params![attempt, redactor.text(log)],
    )
    .context("insert build_log")?;
    Ok(())
}

pub struct FetchedLogs {
    pub entries: Vec<(String, String)>,
    pub attempts_available: i64,
}

pub async fn fetch_failed_logs<C: ConnectionTrait>(
    db: &C,
    logs: &dyn LogStorage,
    evaluation: Uuid,
) -> Result<FetchedLogs> {
    let failed = db
        .query_all_raw(FAILED_ATTEMPT_SQL.bind([sea_orm::Value::Uuid(Some(evaluation))]))
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
        .query_one_raw(ALL_ATTEMPT_SQL.bind([sea_orm::Value::Uuid(Some(evaluation))]))
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

pub fn write_failed_logs(
    conn: &Connection,
    redactor: &Redactor,
    logs: &FetchedLogs,
) -> Result<ManifestRow> {
    create_log_table(conn)?;
    for (attempt, text) in &logs.entries {
        insert_log(conn, redactor, attempt, text)?;
    }

    Ok(ManifestRow {
        table: "build_log".to_owned(),
        rows_included: logs.entries.len() as i64,
        rows_available: logs.attempts_available,
        scope:
            "the evaluation's shared builds, so attempts made for other evaluations are included"
                .to_owned(),
        filter: "failed attempts only".to_owned(),
        redactions: redactor.log_redactions(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ReportOptions, open_report};

    fn redactor(identities: bool, packages: bool) -> Redactor {
        Redactor::new(ReportOptions {
            anonymize_identities: identities,
            anonymize_packages: packages,
            include_logs: true,
            include_instance: true,
        })
    }

    fn stored_log(conn: &Connection) -> String {
        conn.query_row(
            "SELECT log FROM build_log WHERE build_attempt = '0199-attempt'",
            [],
            |r| r.get(0),
        )
        .expect("log")
    }

    #[test]
    fn log_table_stores_readable_text() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = open_report(&dir.path().join("r.db")).expect("open");
        create_log_table(&conn).expect("ddl");
        insert_log(
            &conn,
            &redactor(false, false),
            "0199-attempt",
            "error: builder failed with exit code 1",
        )
        .expect("insert");

        assert_eq!(stored_log(&conn), "error: builder failed with exit code 1");
    }

    #[test]
    fn a_log_is_redacted_on_the_way_in() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conn = open_report(&dir.path().join("r.db")).expect("open");
        create_log_table(&conn).expect("ddl");
        let r = redactor(true, true);
        r.identity("git@example.invalid:acme/infra.git", "repo");

        insert_log(
            &conn,
            &r,
            "0199-attempt",
            "error: building /nix/store/2s7ijz3qblblfb903r4spy3pvd7ag35f-clap_complete-4.6.9 \
             for git@example.invalid:acme/infra.git",
        )
        .expect("insert");

        let text = stored_log(&conn);
        assert!(!text.contains("clap_complete"), "{text}");
        assert!(!text.contains("acme/infra"), "{text}");
        assert!(
            text.contains("2s7ijz3qblblfb903r4spy3pvd7ag35f"),
            "the hash is what a cache check needs: {text}"
        );
    }
}
