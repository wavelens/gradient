/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod concurrency;
mod dedup;
mod gates;
mod skip_marker;

use super::trigger::{TriggerError, trigger_evaluation};
use gradient_entity::evaluation::WalkMode;
use gradient_types::*;
use sea_orm::ConnectionTrait;

pub use gates::{
    park_if_no_cache, park_if_no_workers, park_if_pending_approval, park_storage_full,
};

#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "Created is the hot, immediately-matched happy path; boxing MEvaluation would churn every call/match site for a short-lived return value"
)]
pub enum ApplyOutcome {
    Created {
        evaluation: MEvaluation,
        /// The caller must call `Scheduler::stop_aborted_evaluation` for each evaluation to purge
        /// its `JobTracker` entries, passing the hard-abort flag paired with it.
        aborted_evaluations: Vec<(EvaluationId, bool)>,
    },
    SkippedSameCommit,
    SkippedConcurrency,
    SkippedByCommitMessage,
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error(transparent)]
    Db(#[from] sea_orm::DbErr),
    #[error(transparent)]
    Trigger(#[from] TriggerError),
}

pub struct ApplyInput {
    pub trigger_id: Option<TaskTriggerId>,
    pub trigger_type: TriggerType,
    pub branch: String,
    pub commit_hash: Vec<u8>,
    pub commit_message: Option<String>,
    pub author_name: Option<String>,
    pub manual: bool,
    pub honors_skip_marker: bool,
    pub gate_approval: Option<ApprovalInfo>,
    pub repository_override: Option<String>,
    pub wildcard_override: Option<String>,
    pub source_comment: Option<serde_json::Value>,
    pub started_by: Option<UserId>,
    pub walk_mode: WalkMode,
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
    if head_commit_asks_to_skip(&input) {
        return Ok(ApplyOutcome::SkippedByCommitMessage);
    }

    let mut in_flight =
        gradient_db::evaluations::branch::in_flight_on_branch(db, task.id, &input.branch).await?;

    let newest =
        gradient_db::evaluations::branch::newest_on_branch(db, task.id, &input.branch).await?;

    if dedup::skip_for_same_commit(db, &input, in_flight.as_ref(), newest.as_ref()).await? {
        return Ok(ApplyOutcome::SkippedSameCommit);
    }

    let replaceable_waiting =
        concurrency::find_newest_waiting_evaluation_that_newer_trigger_can_replace(newest.as_ref());
    if replaceable_waiting.is_some() && in_flight.as_ref().map(|e| e.id) == replaceable_waiting {
        in_flight = None;
    }

    let Some(decision) = concurrency::resolve_concurrency(db, task, in_flight).await? else {
        return Ok(ApplyOutcome::SkippedConcurrency);
    };
    let mut aborted_evaluations: Vec<(EvaluationId, bool)> = Vec::new();
    if let Some(waiting) = replaceable_waiting
        && concurrency::abort_superseded_waiting_evaluation(db, waiting).await?
    {
        aborted_evaluations.push((waiting, true));
    }
    aborted_evaluations.extend(
        decision
            .aborted_evaluation
            .map(|id| (id, decision.hard_abort)),
    );

    let eval = match trigger_evaluation(
        db,
        task,
        input.commit_hash,
        input.commit_message,
        input.author_name,
        input.trigger_id,
        decision.concurrent_flag,
        input.branch,
        input.repository_override,
        input.wildcard_override,
        input.source_comment,
        input.started_by,
        input.walk_mode,
    )
    .await
    {
        Ok(e) => e,
        Err(TriggerError::AlreadyInProgress) => return Ok(ApplyOutcome::SkippedConcurrency),
        Err(e) => return Err(e.into()),
    };

    let aborted: Vec<EvaluationId> = aborted_evaluations.iter().map(|(id, _)| *id).collect();
    gradient_db::status::stamp_superseded_by_on_aborted_evaluations(db, &aborted, eval.id).await?;

    let eval = gates::run_gates(db, eval, input.gate_approval.as_ref(), task.project).await?;

    Ok(ApplyOutcome::Created {
        evaluation: eval,
        aborted_evaluations,
    })
}

fn head_commit_asks_to_skip(input: &ApplyInput) -> bool {
    input.honors_skip_marker
        && input
            .commit_message
            .as_deref()
            .is_some_and(skip_marker::commit_requests_skip)
}

#[cfg(test)]
mod tests;
