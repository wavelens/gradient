/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::TriggerError;
use super::flake_snapshot::snapshot_flake_input_overrides;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::consts::NULL_TIME;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, EntityTrait, IntoActiveModel,
    QueryFilter,
};

/// Rejects with [`TriggerError::AlreadyInProgress`] when `task` already has a
/// non-terminal evaluation (Queued / Fetching / EvaluatingFlake /
/// EvaluatingDerivation / Building / Waiting). Shared by the regular trigger and
/// the restart path so both honour the same single-in-flight invariant.
pub(super) async fn ensure_no_active_evaluation<C: ConnectionTrait>(
    db: &C,
    task_id: TaskId,
) -> Result<(), TriggerError> {
    let in_progress = EEvaluation::find()
        .filter(CEvaluation::Task.eq(task_id))
        .filter(
            Condition::any()
                .add(CEvaluation::Status.eq(EvaluationStatus::Queued))
                .add(CEvaluation::Status.eq(EvaluationStatus::Fetching))
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

    Ok(())
}

/// Creates a new `Queued` evaluation for `task` at `commit_hash`.
///
/// - When `concurrent` is false, refuses with [`TriggerError::AlreadyInProgress`]
///   if the task already has a running evaluation (Queued / Fetching /
///   EvaluatingFlake / EvaluatingDerivation / Building / Waiting).
/// - When `concurrent` is true (used by the `all` concurrency policy), skips
///   the in-progress guard and sets `evaluation.concurrent = true` on the new
///   row so the partial unique index lets it through.
/// - Inserts a `Commit` row, then an `Evaluation` row with status `Queued`.
/// - Sets `task.force_evaluation = true` and resets `last_check_at` so the
///   scheduler picks it up immediately on its next tick.
#[allow(
    clippy::too_many_arguments,
    reason = "arg-heavy; refactor tracked in #503"
)]
pub async fn trigger_evaluation<C: ConnectionTrait>(
    db: &C,
    task: &MTask,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
    trigger: Option<gradient_types::ids::TaskTriggerId>,
    concurrent: bool,
    repository_override: Option<String>,
    wildcard_override: Option<String>,
    source_comment: Option<serde_json::Value>,
    started_by: Option<gradient_types::ids::UserId>,
) -> Result<MEvaluation, TriggerError> {
    if !concurrent {
        ensure_no_active_evaluation(db, task.id).await?;
    }

    // Resolve `task.last_evaluation` against the DB so a dangling pointer
    // (eval row gone but the task pointer still set) doesn't trip the
    // `fk-evaluation-previous` foreign key.
    let previous = match task.last_evaluation {
        Some(prev_id) => EEvaluation::find_by_id(prev_id)
            .one(db)
            .await?
            .map(|e| e.id),
        None => None,
    };

    let now = gradient_types::now();

    let acommit = MCommit {
        id: CommitId::now_v7(),
        message: commit_message.unwrap_or_default(),
        hash: commit_hash,
        author_name: author_name.unwrap_or_default(),
        ..Default::default()
    }
    .into_active_model();

    let commit = acommit.insert(db).await?;

    let aevaluation = MEvaluation {
        id: EvaluationId::now_v7(),
        task: Some(task.id),
        repository: repository_override.unwrap_or_else(|| task.repository.clone()),
        commit: commit.id,
        wildcard: wildcard_override.unwrap_or_else(|| task.wildcard.clone()),
        status: EvaluationStatus::Queued,
        previous,
        created_at: now,
        updated_at: now,
        trigger,
        concurrent,
        source_comment,
        started_by,
        ..Default::default()
    }
    .into_active_model();

    let evaluation = aevaluation.insert(db).await?;

    snapshot_flake_input_overrides(db, task.id, evaluation.id).await?;

    let mut atask: ATask = task.clone().into();
    atask.last_check_at = Set(*NULL_TIME);
    atask.last_evaluation = Set(Some(evaluation.id));
    atask.force_evaluation = Set(true);
    atask.update(db).await?;

    Ok(evaluation)
}
