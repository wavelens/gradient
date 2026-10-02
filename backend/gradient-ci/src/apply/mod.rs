/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod concurrency;
mod dedup;
mod gates;

use super::trigger::{TriggerError, trigger_evaluation};
use gradient_entity::evaluation::{EvaluationStatus, WalkMode};
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

pub use gates::{
    park_if_no_cache, park_if_no_workers, park_if_pending_approval, park_if_storage_full,
};

#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "Created is the hot, immediately-matched happy path; boxing MEvaluation would churn every call/match site for a short-lived return value"
)]
pub enum ApplyOutcome {
    Created {
        evaluation: MEvaluation,
        /// The caller must call `Scheduler::cancel_evaluation_jobs` for this evaluation to purge
        /// its `JobTracker` entries.
        aborted_evaluation: Option<EvaluationId>,
        hard_abort: bool,
    },
    SkippedSameCommit,
    SkippedConcurrency,
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error(transparent)]
    Db(#[from] sea_orm::DbErr),
    #[error(transparent)]
    Trigger(#[from] TriggerError),
}

pub struct ApplyInput {
    pub trigger_id: TaskTriggerId,
    pub trigger_type: TriggerType,
    pub commit_hash: Vec<u8>,
    pub commit_message: Option<String>,
    pub author_name: Option<String>,
    pub manual: bool,
    pub gate_approval: Option<ApprovalInfo>,
    pub repository_override: Option<String>,
    pub wildcard_override: Option<String>,
    pub source_comment: Option<serde_json::Value>,
    pub instance_max_storage_gb: i32,
}

#[derive(Debug, Clone)]
pub struct ApprovalInfo {
    pub pr_number: u64,
    pub pr_author: String,
}

pub async fn apply_trigger<C: ConnectionTrait>(
    db: &C,
    task: &MTask,
    input: ApplyInput,
) -> Result<ApplyOutcome, ApplyError> {
    // The in-flight lookup is serving both the commit dedup and the concurrency policy. Concurrent
    // runs such as `input_update` flake bumps are excluded, mirroring the
    // `uq_evaluation_one_active_per_task` partial index. A normal trigger must neither abort nor
    // dedup-block them.
    let active_codes: Vec<i32> = EvaluationStatus::ACTIVE
        .iter()
        .copied()
        .map(i32::from)
        .collect();
    let in_flight = EEvaluation::find()
        .filter(CEvaluation::Task.eq(task.id))
        .filter(CEvaluation::Status.is_in(active_codes))
        .filter(CEvaluation::Concurrent.eq(false))
        .one(db)
        .await?;

    if dedup::skip_for_same_commit(db, task, &input, in_flight.as_ref()).await? {
        return Ok(ApplyOutcome::SkippedSameCommit);
    }

    let Some(decision) = concurrency::resolve_concurrency(db, task, in_flight).await? else {
        return Ok(ApplyOutcome::SkippedConcurrency);
    };

    let eval = match trigger_evaluation(
        db,
        task,
        input.commit_hash,
        input.commit_message,
        input.author_name,
        Some(input.trigger_id),
        decision.concurrent_flag,
        input.repository_override,
        input.wildcard_override,
        input.source_comment,
        None,
        WalkMode::Pruned,
    )
    .await
    {
        Ok(e) => e,
        Err(TriggerError::AlreadyInProgress) => return Ok(ApplyOutcome::SkippedConcurrency),
        Err(TriggerError::Db(ref e))
            if e.to_string().contains("uq_evaluation_one_active_per_task") =>
        {
            return Ok(ApplyOutcome::SkippedConcurrency);
        }
        Err(e) => return Err(e.into()),
    };

    let eval = gates::run_gates(
        db,
        eval,
        input.gate_approval.as_ref(),
        task.project,
        input.instance_max_storage_gb,
    )
    .await?;

    Ok(ApplyOutcome::Created {
        evaluation: eval,
        aborted_evaluation: decision.aborted_evaluation,
        hard_abort: decision.hard_abort,
    })
}

#[cfg(test)]
mod tests;
