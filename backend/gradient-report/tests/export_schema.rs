/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Every export query, run against a real Postgres.
//!
//! The specs are hand-written SQL naming columns one by one, so nothing else in
//! the suite can tell whether they still parse against the schema: a mock
//! connection replays canned rows without ever asking Postgres. Opt in by
//! pointing `GRADIENT_REPORT_TEST_DB` at a database the migrations have run on:
//!
//! ```text
//! initdb -D "$PGDATA" -U postgres --auth=trust && pg_ctl -D "$PGDATA" start
//! createdb -U postgres report_test
//! DATABASE_URL=postgres://postgres@localhost/report_test \
//!   cargo run -p gradient-migration -- up
//! GRADIENT_REPORT_TEST_DB=postgres://postgres@localhost/report_test \
//!   cargo test -p gradient-report --test export_schema
//! ```

use gradient_report::{eval_scope_tables, fetch_rows, instance_tables};
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};

/// The two log queries live in `logs.rs` behind a `LogStorage`, which this test
/// has no business standing up; the SQL is what needs the schema check.
const LOG_SQL: [(&str, &str); 2] = [
    (
        "failed attempts",
        "SELECT a.id::text FROM build_attempt a \
         WHERE a.derivation_build IN (SELECT derivation_build FROM build_job WHERE evaluation = $1) \
           AND a.outcome IN (3, 4)",
    ),
    (
        "count attempts",
        "SELECT count(*)::text FROM build_attempt a \
         WHERE a.derivation_build IN (SELECT derivation_build FROM build_job WHERE evaluation = $1)",
    ),
];

#[test]
fn every_export_query_runs_against_the_migrated_schema() {
    let Ok(url) = std::env::var("GRADIENT_REPORT_TEST_DB") else {
        eprintln!("skipped: set GRADIENT_REPORT_TEST_DB to a migrated database");
        return;
    };

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async move {
            let db = Database::connect(url).await.expect("connect");
            // An id that matches nothing still type-checks every predicate,
            // which is the half that broke: `uuid = text` is not an operator.
            let scope = Uuid::now_v7();
            let mut failures = Vec::new();

            for spec in eval_scope_tables().iter().chain(instance_tables()) {
                if let Err(e) = fetch_rows(&db, spec, scope).await {
                    failures.push(format!("{}: {e:#}", spec.name));
                }
            }

            for (name, sql) in LOG_SQL {
                let result = db
                    .query_all_raw(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        sql,
                        [sea_orm::Value::Uuid(Some(scope))],
                    ))
                    .await;
                if let Err(e) = result {
                    failures.push(format!("{name}: {e:#}"));
                }
            }

            assert!(failures.is_empty(), "{}", failures.join("\n"));
        });
}
