/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pure database operations for the `evaluation` and `evaluation_message` tables.
//!
//! [`EvalRepo`] takes only a `&DatabaseConnection` — no `Arc<ServerState>`.
//! Side effects (webhooks, CI reporting) stay in the service layer.

use anyhow::Result;

use entity::evaluation::EvaluationStatus;
use entity::evaluation_message::MessageLevel;
use sea_orm::DatabaseConnection;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, QueryFilter};

use crate::types::*;

pub struct EvalRepo<'db> {
    db: &'db DatabaseConnection,
}

impl<'db> EvalRepo<'db> {
    pub fn new(db: &'db DatabaseConnection) -> Self {
        Self { db }
    }

    /// Fetch the current DB row for this evaluation.
    pub async fn find(&self, id: EvaluationId) -> Result<Option<MEvaluation>> {
        Ok(EEvaluation::find_by_id(id).one(self.db).await?)
    }

    /// Atomically update the evaluation status, guarding against overwriting
    /// a terminal state. Returns the number of rows affected (0 = already terminal).
    pub async fn update_status_guarded(
        &self,
        id: EvaluationId,
        status: EvaluationStatus,
    ) -> Result<u64> {
        let now = crate::types::now();
        let res = EEvaluation::update_many()
            .col_expr(CEvaluation::Status, sea_orm::sea_query::Expr::value(status))
            .col_expr(CEvaluation::UpdatedAt, sea_orm::sea_query::Expr::value(now))
            .filter(CEvaluation::Id.eq(id))
            .filter(
                Condition::all()
                    .add(CEvaluation::Status.ne(EvaluationStatus::Aborted))
                    .add(CEvaluation::Status.ne(EvaluationStatus::Failed))
                    .add(CEvaluation::Status.ne(EvaluationStatus::Completed)),
            )
            .exec(self.db)
            .await?;
        Ok(res.rows_affected)
    }

    /// Insert a single evaluation message row (error, warning, or info).
    pub async fn insert_message(
        &self,
        evaluation_id: EvaluationId,
        level: MessageLevel,
        message: String,
        source: Option<String>,
    ) -> Result<()> {
        let msg = AEvaluationMessage {
            id: Set(EvaluationMessageId::now_v7()),
            evaluation: Set(evaluation_id),
            level: Set(level),
            message: Set(message),
            source: Set(source),
            created_at: Set(crate::types::now()),
        };
        EEvaluationMessage::insert(msg).exec(self.db).await?;
        Ok(())
    }

    /// Insert multiple evaluation message rows in one batch.
    pub async fn insert_messages(
        &self,
        evaluation_id: EvaluationId,
        messages: Vec<(MessageLevel, String, Option<String>)>,
    ) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let now = crate::types::now();
        let rows: Vec<AEvaluationMessage> = messages
            .into_iter()
            .map(|(level, message, source)| AEvaluationMessage {
                id: Set(EvaluationMessageId::now_v7()),
                evaluation: Set(evaluation_id),
                level: Set(level),
                message: Set(message),
                source: Set(source),
                created_at: Set(now),
            })
            .collect();
        EEvaluationMessage::insert_many(rows).exec(self.db).await?;
        Ok(())
    }

    /// Mark an evaluation as aborted, returning its in-progress builds.
    pub async fn find_active_builds_for_evaluation(
        &self,
        evaluation_id: EvaluationId,
    ) -> Result<Vec<MBuild>> {
        use entity::build::BuildStatus;
        let builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation_id))
            .filter(
                Condition::any()
                    .add(CBuild::Status.eq(BuildStatus::Created))
                    .add(CBuild::Status.eq(BuildStatus::Queued))
                    .add(CBuild::Status.eq(BuildStatus::Building)),
            )
            .all(self.db)
            .await?;
        Ok(builds)
    }

    /// Transition all `Created` builds for an evaluation to `Queued`.
    /// Returns the count of builds transitioned.
    pub async fn transition_created_builds_to_queued(
        &self,
        evaluation_id: EvaluationId,
    ) -> Result<Vec<MBuild>> {
        use entity::build::BuildStatus;
        let builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation_id))
            .filter(CBuild::Status.eq(BuildStatus::Created))
            .all(self.db)
            .await?;
        Ok(builds)
    }
}
