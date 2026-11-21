/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::ids::EvaluationId;
use sea_orm::{DbErr, RuntimeErr};
use thiserror::Error;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum MissingRow {
    #[error("evaluation {id} not found")]
    Evaluation { id: EvaluationId },
    #[error("unique violation but no active row found")]
    ActiveAdminTask,
}

/// Warning logs omit the Postgres message, since that message can contain a literal value
/// written into the SQL statement.
pub fn database_error_kind_text_and_sqlstate_for_warning_log(
    err: &anyhow::Error,
) -> (&'static str, Option<String>) {
    let Some(db_err) = err.chain().find_map(|cause| cause.downcast_ref::<DbErr>()) else {
        return ("not a database error", None);
    };
    let (kind, runtime) = match db_err {
        DbErr::ConnectionAcquire(_) => ("database connection acquire error", None),
        DbErr::Conn(runtime) => ("database connection failed", Some(runtime)),
        DbErr::Exec(runtime) => ("database statement failed", Some(runtime)),
        DbErr::Query(runtime) => ("error for database query", Some(runtime)),
        _ => ("other database errors", None),
    };
    let sqlstate = match runtime {
        Some(RuntimeErr::SqlxError(e)) => e
            .as_database_error()
            .and_then(|e| e.code())
            .map(|code| code.into_owned()),
        _ => None,
    };

    (kind, sqlstate)
}
