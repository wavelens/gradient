/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::{DatabaseBackend, DatabaseConnection, IntoMockRow, MockDatabase};

/// A `MockDatabase` that answers the next `SELECT` with `rows`.
/// Chain `.append_query_results` manually when a handler makes multiple queries.
pub fn db_with<T: IntoMockRow>(rows: Vec<T>) -> DatabaseConnection {
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([rows])
        .into_connection()
}
