/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Orchestrates trigger fire → evaluation creation. Encapsulates commit-level
//! deduplication ([`dedup`]), concurrency policy ([`concurrency`]), and the
//! post-creation parking [`gates`]. Callers: scheduler dispatch loop, forge
//! webhooks, manual API endpoints.

mod concurrency;
mod dedup;
mod gates;

use super::trigger::{TriggerError, trigger_evaluation};
use gradient_types::*;
use gradient_entity::evaluation::EvaluationStatus;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

pub use gates::{
    park_if_no_cache, park_if_no_workers, park_if_pending_approval, park_if_storage_full,
};

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum ApplyOutcome {
    Created {
        evaluation: MEvaluation,
        /// `Some(eval_id)` if a concurrency policy aborted an in-flight eval.
        /// The caller is responsible for calling `Scheduler::cancel_evaluation_jobs`
        /// for that eval to purge its in-memory `JobTracker` entries.
        aborted_evaluation: Option<EvaluationId>,
        /// `derivation_build` anchors marked `Aborted` by a `HardAbort` policy.
        /// Empty for `SoftAbort` (builds keep running) and the no-abort path.
        aborted_anchors: Vec<DerivationBuildId>,
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
    pub trigger_id: ProjectTriggerId,
    pub trigger_type: TriggerType,
    pub commit_hash: Vec<u8>,
    pub commit_message: Option<String>,
    pub author_name: Option<String>,
    /// Set true for manual UI re-runs and `/triggers/{id}/test` calls.
    /// Bypasses the same-commit dedup check.
    pub manual: bool,
    /// Set by the PR webhook layer when the caller has determined the PR
    /// requires maintainer approval (untrusted contributor on a require_approval
    /// trigger). `apply_trigger` parks the resulting evaluation in
    /// `Waiting + WaitingReason::Approval` instead of `Queued`.
    pub gate_approval: Option<ApprovalInfo>,
    /// Override the evaluation's `repository` URL. Used by the PR webhook layer
    /// so commits on a fork are fetched from the fork's clone URL instead of
    /// `project.repository` (which only has the base repo's history). `None`
    /// falls back to `project.repository`.
    pub repository_override: Option<String>,
    /// Override the evaluation's `wildcard` attribute pattern. Used by
    /// `/gradient run <wildcard>` so a maintainer can re-target a single
    /// run without editing project config. `None` falls back to
    /// `project.wildcard`.
    pub wildcard_override: Option<String>,
    /// Records the PR comment that triggered this evaluation. Persisted
    /// in `evaluation.source_comment` so the terminal-status reporter can
    /// react with thumbs-up / thumbs-down once the build resolves.
    pub source_comment: Option<serde_json::Value>,
    /// Instance-wide `max_storage_gb` limit (`GRADIENT_MAX_STORAGE_GB`), used by
    /// the storage-full gate. `0` disables the instance-wide limit.
    pub instance_max_storage_gb: i32,
}

/// Identification of the pull request a maintainer must approve before the
/// evaluation runs. Persisted on `evaluation.waiting_reason`.
#[derive(Debug, Clone)]
pub struct ApprovalInfo {
    pub pr_number: u64,
    pub pr_author: String,
}

pub async fn apply_trigger<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
    input: ApplyInput,
) -> Result<ApplyOutcome, ApplyError> {
    // Find any in-flight evaluation up-front; we use it for dedup against the
    // currently-running commit AND for the concurrency policy below.
    let active_codes: Vec<i32> = EvaluationStatus::ACTIVE
        .iter()
        .copied()
        .map(i32::from)
        .collect();
    let in_flight = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .filter(CEvaluation::Status.is_in(active_codes))
        .one(db)
        .await?;

    if dedup::skip_for_same_commit(db, project, &input, in_flight.as_ref()).await? {
        return Ok(ApplyOutcome::SkippedSameCommit);
    }

    let Some(decision) = concurrency::resolve_concurrency(db, project, in_flight).await? else {
        return Ok(ApplyOutcome::SkippedConcurrency);
    };

    let eval = match trigger_evaluation(
        db,
        project,
        input.commit_hash,
        input.commit_message,
        input.author_name,
        Some(input.trigger_id),
        decision.concurrent_flag,
        input.repository_override,
        input.wildcard_override,
        input.source_comment,
        None,
    )
    .await
    {
        Ok(e) => e,
        Err(TriggerError::AlreadyInProgress) => return Ok(ApplyOutcome::SkippedConcurrency),
        Err(TriggerError::Db(ref e))
            if e.to_string()
                .contains("uq_evaluation_one_active_per_project") =>
        {
            return Ok(ApplyOutcome::SkippedConcurrency);
        }
        Err(e) => return Err(e.into()),
    };

    let eval = gates::run_gates(
        db,
        eval,
        input.gate_approval.as_ref(),
        project.organization,
        input.instance_max_storage_gb,
    )
    .await?;

    Ok(ApplyOutcome::Created {
        evaluation: eval,
        aborted_evaluation: decision.aborted_evaluation,
        aborted_anchors: decision.aborted_anchors,
    })
}

#[cfg(test)]
mod tests;
