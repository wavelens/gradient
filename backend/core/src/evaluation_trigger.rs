/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared logic for creating a queued evaluation from any trigger source
//! (API endpoint, incoming forge webhook, …).

use crate::consts::NULL_TIME;
use crate::types::*;
use chrono::Utc;
use entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum TriggerError {
    #[error("evaluation already in progress for this project")]
    AlreadyInProgress,
    #[error("database error: {0}")]
    Db(#[from] sea_orm::DbErr),
}

/// Creates a new `Queued` evaluation for `project` at `commit_hash`.
///
/// - Refuses with [`TriggerError::AlreadyInProgress`] when the project already
///   has a running evaluation (Queued / EvaluatingFlake / EvaluatingDerivation /
///   Building / Waiting).
/// - Inserts a `Commit` row, then an `Evaluation` row with status `Queued`.
/// - Sets `project.force_evaluation = true` and resets `last_check_at` so the
///   scheduler picks it up immediately on its next tick.
pub async fn trigger_evaluation(
    db: &DatabaseConnection,
    project: &MProject,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
) -> Result<MEvaluation, TriggerError> {
    // Guard: reject if an evaluation is already in progress.
    let in_progress = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .filter(
            Condition::any()
                .add(CEvaluation::Status.eq(EvaluationStatus::Queued))
                .add(CEvaluation::Status.eq(EvaluationStatus::EvaluatingFlake))
                .add(CEvaluation::Status.eq(EvaluationStatus::EvaluatingDerivation))
                .add(CEvaluation::Status.eq(EvaluationStatus::Building))
                .add(CEvaluation::Status.eq(EvaluationStatus::Waiting)),
        )
        .one(db)
        .await?;

    if in_progress.is_some() {
        return Err(TriggerError::AlreadyInProgress);
    }

    let now = Utc::now().naive_utc();

    let acommit = ACommit {
        id: Set(Uuid::new_v4()),
        message: Set(commit_message.unwrap_or_default()),
        hash: Set(commit_hash),
        author: Set(None),
        author_name: Set(author_name.unwrap_or_default()),
    };
    let commit = acommit.insert(db).await?;

    let aevaluation = AEvaluation {
        id: Set(Uuid::new_v4()),
        project: Set(Some(project.id)),
        repository: Set(project.repository.clone()),
        commit: Set(commit.id),
        wildcard: Set(project.evaluation_wildcard.clone()),
        status: Set(EvaluationStatus::Queued),
        previous: Set(project.last_evaluation),
        next: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    let evaluation = aevaluation.insert(db).await?;

    let mut aproject: AProject = project.clone().into();
    aproject.last_check_at = Set(*NULL_TIME);
    aproject.last_evaluation = Set(Some(evaluation.id));
    aproject.force_evaluation = Set(true);
    aproject.update(db).await?;

    Ok(evaluation)
}
