/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub use gradient_api_types::HistoryBar;
use gradient_db::dashboard::histories;
use gradient_types::EvaluationId;
use sea_orm::{ConnectionTrait, DbErr};
use std::collections::HashMap;

pub async fn get_last_n_evaluation_runs_by_latest_evaluation_id<C: ConnectionTrait>(
    db: &C,
    latest: &[EvaluationId],
    n: u64,
) -> Result<HashMap<EvaluationId, Vec<HistoryBar>>, DbErr> {
    Ok(histories(db, latest, n)
        .await?
        .into_iter()
        .map(|(id, rows)| (id, rows.into_iter().map(HistoryBar::from).collect()))
        .collect())
}
