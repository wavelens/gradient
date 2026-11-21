/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod storage;

use gradient_db::status::change_evaluation_status;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::waiting_reason::WaitingReason;
use gradient_types::*;
use sea_orm::ConnectionTrait;
use sea_orm::sea_query::Expr;

pub use storage::park_storage_full;

async fn park<C: ConnectionTrait>(
    db: &C,
    eval: MEvaluation,
    reason: WaitingReason,
) -> Result<MEvaluation, sea_orm::DbErr> {
    let parked = change_evaluation_status(db, eval.id, EvaluationStatus::PARK, |update| {
        update.col_expr(
            CEvaluation::WaitingReason,
            Expr::value(Some(reason.to_json())),
        )
    })
    .await?;
    Ok(parked.unwrap_or(eval))
}
