/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_db::dashboard::{HistoryRow, histories};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::EvaluationId;
use sea_orm::{ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct HistoryBar {
    pub id: EvaluationId,
    pub status: EvaluationStatus,
    pub duration_ms: Option<i64>,
    pub created_at: NaiveDateTime,
    pub started_at: Option<NaiveDateTime>,
    pub finished_at: Option<NaiveDateTime>,
}

impl From<HistoryRow> for HistoryBar {
    fn from(h: HistoryRow) -> Self {
        Self {
            id: h.id,
            status: h.status,
            duration_ms: h.duration_ms,
            created_at: h.created_at,
            started_at: h.started_at,
            finished_at: h.finished_at,
        }
    }
}

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
