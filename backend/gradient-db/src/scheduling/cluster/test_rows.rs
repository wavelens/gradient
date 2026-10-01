/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::{MockExecResult, Statement};

pub(super) fn exec(rows_affected: u64) -> MockExecResult {
    MockExecResult {
        last_insert_id: 0,
        rows_affected,
    }
}

pub(super) fn logged(db: sea_orm::DatabaseConnection) -> Vec<Statement> {
    db.into_transaction_log()
        .iter()
        .flat_map(|t| t.statements().to_vec())
        .collect()
}
