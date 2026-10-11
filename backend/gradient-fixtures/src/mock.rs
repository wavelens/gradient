/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::{
    DatabaseBackend, DatabaseConnection, MockDatabase, MockExecResult, Statement, Transaction,
};

pub fn mock_db() -> MockDatabase {
    MockDatabase::new(DatabaseBackend::Postgres)
}

pub fn empty_db() -> DatabaseConnection {
    mock_db().into_connection()
}

pub fn exec_ok() -> MockExecResult {
    MockExecResult {
        last_insert_id: 0,
        rows_affected: 1,
    }
}

pub fn all_statements(log: Vec<Transaction>) -> Vec<Statement> {
    log.iter().flat_map(|t| t.statements().to_vec()).collect()
}

pub fn ran(db: DatabaseConnection, prefix: &str) -> bool {
    all_statements(db.into_transaction_log())
        .iter()
        .any(|s| s.sql.starts_with(prefix))
}

/// A `MockDatabase` is recording a whole transaction as one entry.
/// sea-orm is bracketing that entry with a synthetic `BEGIN` and `COMMIT`.
/// This list is dropping them, unlike a test asserting on transaction control.
pub fn statements(log: Vec<Transaction>) -> Vec<String> {
    raw_statements(log)
        .iter()
        .map(|s| format!("{s:?}"))
        .collect()
}

pub fn raw_statements(log: Vec<Transaction>) -> Vec<Statement> {
    log.iter()
        .flat_map(|t| t.statements().iter())
        .filter(|s| !is_transaction_control(&s.sql))
        .cloned()
        .collect()
}

fn is_transaction_control(sql: &str) -> bool {
    let sql = sql.trim().to_uppercase();
    matches!(sql.as_str(), "BEGIN" | "COMMIT" | "ROLLBACK")
        || sql.starts_with("SAVEPOINT ")
        || sql.starts_with("RELEASE SAVEPOINT ")
        || sql.starts_with("ROLLBACK TO SAVEPOINT ")
}
