/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Handles `BuildOutput` messages from workers and build job lifecycle.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::{Context, Result};

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_db::{
    collect_transitive_dependents, fail_latest_attempt, update_build_status,
    update_evaluation_status,
};
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
};
use tracing::{error, info, warn};

use super::jobs::PendingBuildJob;
use gradient_types::BuildOutputMetadata;
use gradient_types::proto::BuildFailureKind;
use gradient_types::proto::BuildMetrics;
use gradient_types::proto::BuildOutput;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureOutcome {
    Retry,
    Permanent,
    Timeout,
    /// Penalty-free re-queue (substitute miss): back to `Queued` without
    /// bumping `attempt`. Escalation to a real build is decided at dispatch.
    Requeue,
}

/// Decide what to do with a failed build given its classification and how many
/// attempts it has already had (`attempt` is the count *before* this failure).
pub(crate) fn decide_failure_outcome(
    kind: BuildFailureKind,
    attempt: i32,
    max_attempts: u32,
) -> FailureOutcome {
    match kind {
        BuildFailureKind::Timeout => FailureOutcome::Timeout,
        BuildFailureKind::Permanent => FailureOutcome::Permanent,
        BuildFailureKind::SubstituteUnavailable => FailureOutcome::Requeue,
        BuildFailureKind::Transient => {
            if (attempt + 1) < max_attempts as i32 {
                FailureOutcome::Retry
            } else {
                FailureOutcome::Permanent
            }
        }
    }
}

/// Best-effort mapping from the worker's failure classification to a stored
/// `build_attempt.reason`. `Transient` has no single cause, so it stays `None`.
fn attempt_reason(kind: BuildFailureKind) -> Option<AttemptFailureReason> {
    match kind {
        BuildFailureKind::SubstituteUnavailable => Some(AttemptFailureReason::SubstituteUnavailable),
        BuildFailureKind::Permanent => Some(AttemptFailureReason::BuilderNonzero),
        BuildFailureKind::Timeout => Some(AttemptFailureReason::WallClockTimeout),
        BuildFailureKind::Transient => None,
    }
}

/// True when a `FailedTransient` build's exponential backoff window has elapsed
/// and it is due for re-queue. `attempt` is `>= 1` (it failed at least once);
/// window = `base_secs * 2^(attempt-1)`.
pub(crate) fn retry_backoff_elapsed(
    attempt: i32,
    failed_at: chrono::NaiveDateTime,
    now: chrono::NaiveDateTime,
    base_secs: u64,
) -> bool {
    let shift = (attempt.max(1) - 1).min(16) as u32;
    let window = base_secs.saturating_mul(1u64 << shift);
    (now - failed_at).num_seconds() >= window as i64
}

/// Wraps `&ServerState` so build-lifecycle helpers don't repeat `state` as a parameter.
pub(crate) struct BuildStateHandler<'a> {
    state: &'a Arc<ServerState>,
}

impl<'a> BuildStateHandler<'a> {
    pub(crate) fn new(state: &'a Arc<ServerState>) -> Self {
        Self { state }
    }

    pub async fn handle_build_output(
        &self,
        _job: &PendingBuildJob,
        build_id: BuildId,
        outputs: Vec<BuildOutput>,
        metrics: Option<BuildMetrics>,
        substituted: bool,
    ) -> Result<()> {
        let build = EBuild::find_by_id(build_id)
            .one(&self.state.worker_db)
            .await
            .context("fetch build")?
            .with_context(|| format!("build {} not found", build_id))?;

        let derivation_id = build.derivation;

        // Per-build metrics: a multi-build job yields one `BuildOutput` per
        // build, so this records exactly one `derivation_metric` row per build.
        if let Some(metrics) = metrics {
            self.record_metrics(&build, derivation_id, &metrics)
                .await;
        }

        for output in &outputs {
            let existing = EDerivationOutput::find()
                .filter(CDerivationOutput::Derivation.eq(derivation_id))
                .filter(CDerivationOutput::Name.eq(&output.name))
                .one(&self.state.worker_db)
                .await
                .context("fetch derivation_output")?;

            if let Some(row) = existing {
                let row_id = row.id;
                let mut active = row.into_active_model();
                if let BuildOutputMetadata::Available {
                    nar_size,
                    nar_hash: _,
                } = output.nar_metadata()
                {
                    active.nar_size = Set(Some(nar_size));
                }
                if let Err(e) = active.update(&self.state.worker_db).await {
                    error!(error = %e, %build_id, output_name = %output.name, "failed to update derivation_output");
                }

                // Delete any prior products for this output (idempotency on retry).
                if let Err(e) = EBuildProduct::delete_many()
                    .filter(CBuildProduct::DerivationOutput.eq(row_id))
                    .exec(&self.state.worker_db)
                    .await
                    .context("delete prior build_product rows")
                {
                    warn!(error = %e, %build_id, output_name = %output.name, "failed to delete prior build_product rows");
                }

                // Insert new product rows.
                for product in &output.products {
                    let am = ABuildProduct {
                        id: Set(BuildProductId::now_v7()),
                        derivation_output: Set(row_id),
                        file_type: Set(product.file_type.clone()),
                        subtype: Set(product.subtype.clone()),
                        name: Set(product.name.clone()),
                        path: Set(product.path.clone()),
                        size: Set(product.size.map(|s| s as i64)),
                        created_at: Set(gradient_types::now()),
                    };
                    if let Err(e) = am.insert(&self.state.worker_db).await {
                        warn!(error = %e, %build_id, output_name = %output.name, "failed to insert build_product");
                    }
                }
            } else {
                warn!(%build_id, output_name = %output.name, "derivation_output row not found");
            }
        }

        info!(%build_id, output_count = outputs.len(), "build outputs recorded");

        // The daemon found the outputs already valid - no build ran. Mark the
        // build `Substituted` here so the later `JobCompleted` finalizes it as
        // such instead of `Completed` (issue #303).
        if substituted {
            update_build_status(&self.state.db(), build, BuildStatus::Substituted).await;
        }

        Ok(())
    }

    pub async fn handle_build_job_completed(&self, build_id: BuildId) -> Result<()> {
        let build = match EBuild::find_by_id(build_id)
            .one(&self.state.worker_db)
            .await?
        {
            Some(b) => b,
            None => {
                warn!(%build_id, "build not found on job_completed");
                return Ok(());
            }
        };
        let evaluation_id = build.evaluation;
        let derivation_id = build.derivation;
        let was_external_cached = build.substitutable;

        // `handle_build_output` already moved the build to `Substituted` when the
        // outputs were found already valid; preserve that terminal state instead
        // of overwriting it with `Completed`.
        let terminal = if build.status == BuildStatus::Substituted {
            BuildStatus::Substituted
        } else {
            BuildStatus::Completed
        };
        let leader = update_build_status(&self.state.db(), build, terminal).await;
        self.propagate_to_followers(&leader).await?;

        if was_external_cached {
            let state = Arc::clone(self.state);
            let leader_id = leader.id;
            tokio::spawn(async move {
                let drv_path = match EDerivation::find_by_id(derivation_id)
                    .one(&state.worker_db)
                    .await
                {
                    Ok(Some(d)) => d.drv_path(),
                    Ok(None) => {
                        warn!(%leader_id, %derivation_id, "substitute_log: derivation row missing");
                        return;
                    }
                    Err(e) => {
                        warn!(%leader_id, error = %e, "substitute_log: derivation lookup failed");
                        return;
                    }
                };
                if let Err(e) = crate::log_substitution::substitute_log(
                    state,
                    leader_id,
                    derivation_id,
                    drv_path,
                    true,
                )
                .await
                {
                    warn!(%leader_id, error = %e, "substitute_log spawn failed");
                }
            });
        }

        self.check_evaluation_done(evaluation_id).await
    }

    /// Insert a `derivation_metric` history row from a build's worker metrics.
    /// Called once per build from the `BuildOutput` handler.
    async fn record_metrics(
        &self,
        build: &MBuild,
        derivation_id: DerivationId,
        metrics: &BuildMetrics,
    ) {
        let (pname, closure_size) = match EDerivation::find_by_id(derivation_id)
            .one(&self.state.worker_db)
            .await
        {
            Ok(Some(d)) => (d.pname, d.closure_size),
            Ok(None) => {
                warn!(%derivation_id, "derivation row missing; skipping metric history");
                return;
            }
            Err(e) => {
                warn!(%derivation_id, error = %e, "derivation lookup failed; skipping metric history");
                return;
            }
        };

        let metric = ADerivationMetric {
            id: Set(DerivationMetricId::now_v7()),
            derivation: Set(derivation_id),
            pname: Set(pname),
            closure_size: Set(closure_size),
            peak_ram_mb: Set(metrics.peak_ram_mb.map(|v| v as i64)),
            cpu_time_ms: Set(metrics.cpu_time_ms.map(|v| v as i64)),
            avg_cpu_pct: Set(metrics.avg_cpu_pct.map(|v| v as f64)),
            disk_read_bytes: Set(metrics.disk_read_bytes.map(|v| v as i64)),
            disk_write_bytes: Set(metrics.disk_write_bytes.map(|v| v as i64)),
            peak_network_mbps: Set(metrics.peak_network_mbps.map(|v| v as f64)),
            oom_killed: Set(metrics.oom_killed),
            build_time_ms: Set(metrics.build_time_ms.map(|v| v as i64)),
            worker_id: Set(gradient_db::latest_attempt_worker(&self.state.worker_db, build.id)
                .await
                .ok()
                .flatten()
                .unwrap_or_default()),
            created_at: Set(gradient_types::now()),
        };

        if let Err(e) = metric.insert(&self.state.worker_db).await {
            warn!(%derivation_id, error = %e, "failed to record derivation_metric");
        }
    }

    pub async fn handle_build_job_failed(
        &self,
        build_id: BuildId,
        error: &str,
        kind: BuildFailureKind,
    ) -> Result<()> {
        let build = match EBuild::find_by_id(build_id)
            .one(&self.state.worker_db)
            .await?
        {
            Some(b) => b,
            None => {
                warn!(%build_id, "build not found on job_failed");
                return Ok(());
            }
        };

        // Surface the worker's failure reason in the build log so the
        // frontend's log viewer renders it. Without this, pre-`nix build`
        // aborts (prefetch-time errors, daemon connection failures, etc.)
        // produce a Failed badge with an empty log - useless for diagnosis.
        let log_id = gradient_db::latest_attempt_log_id(&self.state.worker_db, build.id)
            .await
            .unwrap_or(build.id);
        if let Err(e) = self
            .state
            .log_storage
            .append(log_id, &format!("\n=== build failed: {error} ===\n"))
            .await
        {
            warn!(%build_id, error = %e, "failed to append worker error to build log");
        }

        let evaluation_id = build.evaluation;
        let derivation_id = build.derivation;
        let attempt = build.attempt;
        let max_attempts = self.state.config.eval.build_max_attempts;

        if let Err(e) = fail_latest_attempt(
            &self.state.worker_db,
            build_id,
            AttemptOutcome::Failed,
            attempt_reason(kind),
        )
        .await
        {
            warn!(%build_id, error = %e, "failed to record attempt failure reason");
        }

        match decide_failure_outcome(kind, attempt, max_attempts) {
            FailureOutcome::Retry => {
                let mut active: ABuild = build.clone().into_active_model();
                active.attempt = Set(attempt + 1);
                if let Err(e) = active.update(&self.state.worker_db).await {
                    error!(%build_id, error = %e, "failed to bump build attempt");
                }
                let reloaded = EBuild::find_by_id(build_id)
                    .one(&self.state.worker_db)
                    .await?
                    .unwrap_or(build);
                update_build_status(&self.state.db(), reloaded, BuildStatus::FailedTransient)
                    .await;
                info!(%build_id, attempt = attempt + 1, "transient build failure; scheduled for retry");
                return Ok(());
            }
            FailureOutcome::Requeue => {
                // Substitute miss: back to the queue without an `attempt` bump
                // or a permanent mark. Dispatch escalates to a real build once
                // the substitute-miss count crosses the threshold. Followers and
                // dependents are untouched - nothing failed.
                update_build_status(&self.state.db(), build, BuildStatus::Queued).await;
                info!(%build_id, "substitute unavailable; re-queued for re-dispatch/escalation");
                return Ok(());
            }
            FailureOutcome::Permanent => {
                let leader =
                    update_build_status(&self.state.db(), build, BuildStatus::FailedPermanent)
                        .await;
                self.propagate_to_followers(&leader).await?;
            }
            FailureOutcome::Timeout => {
                let leader =
                    update_build_status(&self.state.db(), build, BuildStatus::FailedTimeout)
                        .await;
                self.propagate_to_followers(&leader).await?;
            }
        }
        self.cascade_dependency_failed(evaluation_id, derivation_id)
            .await?;
        self.check_evaluation_done(evaluation_id).await
    }

    /// Resolve the fate of every build held behind `leader` via `via = leader.id`.
    ///
    /// On leader SUCCESS (`Completed`/`Substituted`) the followers are RELEASED
    /// back to the queue (`status = Queued`, `via = None`) so they are
    /// re-dispatched: nix transparently substitutes the leader's now-cached
    /// output and each follower registers its own artefacts + `build_attempt`.
    ///
    /// On leader FAILURE (`FailedPermanent`/`FailedTimeout`/`DependencyFailed`)
    /// the failure is propagated onto each follower and its dependents are
    /// cascaded, exactly as before.
    ///
    /// `Aborted` is not propagated - when a leader is aborted (its eval was
    /// cancelled), callers re-elect a new leader from the followers instead.
    async fn propagate_to_followers(&self, leader: &MBuild) -> Result<()> {
        let is_success = matches!(
            leader.status,
            BuildStatus::Completed | BuildStatus::Substituted
        );
        let is_failure = matches!(
            leader.status,
            BuildStatus::FailedPermanent
                | BuildStatus::FailedTimeout
                | BuildStatus::DependencyFailed
        );
        if !is_success && !is_failure {
            return Ok(());
        }

        let followers = EBuild::find()
            .filter(CBuild::Via.eq(leader.id))
            .all(&self.state.worker_db)
            .await
            .context("fetch followers")?;
        if followers.is_empty() {
            return Ok(());
        }

        if is_success {
            let now = gradient_types::now();
            for follower in followers {
                let follower_id = follower.id;
                let mut active: ABuild = follower.into_active_model();
                active.status = Set(BuildStatus::Queued);
                active.via = Set(None);
                active.updated_at = Set(now);
                if let Err(e) = active.update(&self.state.worker_db).await {
                    error!(error = %e, %follower_id, "failed to release follower for re-dispatch");
                }
            }

            return Ok(());
        }

        for follower in followers {
            let evaluation_id = follower.evaluation;
            let derivation_id = follower.derivation;
            let mut active: ABuild = follower.clone().into_active_model();
            active.via = Set(None);
            if let Err(e) = active.update(&self.state.worker_db).await {
                error!(error = %e, follower_id = %follower.id, "failed to clear follower via");
                continue;
            }

            let Some(reloaded) = EBuild::find_by_id(follower.id)
                .one(&self.state.worker_db)
                .await?
            else {
                continue;
            };
            update_build_status(&self.state.db(), reloaded, leader.status).await;

            if matches!(
                leader.status,
                BuildStatus::FailedPermanent
                    | BuildStatus::FailedTimeout
                    | BuildStatus::DependencyFailed
            ) {
                self.cascade_dependency_failed(evaluation_id, derivation_id)
                    .await?;
            }
            self.check_evaluation_done(evaluation_id).await?;
        }

        Ok(())
    }

    async fn cascade_dependency_failed(
        &self,
        evaluation_id: EvaluationId,
        failed_derivation_id: DerivationId,
    ) -> Result<()> {
        let mut closure =
            collect_transitive_dependents(&self.state.worker_db, failed_derivation_id).await?;
        // The failed derivation itself was already marked Failed by the caller;
        // only its dependents need DependencyFailed.
        closure.remove(&failed_derivation_id);
        if closure.is_empty() {
            return Ok(());
        }

        let closure_ids: Vec<DerivationId> = closure.into_iter().collect();
        let db = &self.state.worker_db;
        let cascaded_builds = gradient_db::fetch_in_chunks(&closure_ids, |chunk| async move {
            EBuild::find()
                .filter(CBuild::Evaluation.eq(evaluation_id))
                .filter(CBuild::Status.is_in(vec![
                    BuildStatus::Created,
                    BuildStatus::Queued,
                    BuildStatus::FailedTransient,
                ]))
                .filter(CBuild::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .context("fetch builds for cascade")?;

        for build in cascaded_builds {
            update_build_status(&self.state.db(), build, BuildStatus::DependencyFailed).await;
        }
        Ok(())
    }

    /// Transitions the evaluation to its final state if all builds are done.
    ///
    /// Returns early if any build is still active (Created/Queued/Building) or if
    /// the evaluation is not in `Building` state. Otherwise sets `Failed` when at
    /// least one build is a terminal failure (FailedPermanent, FailedTimeout, or
    /// DependencyFailed), else `Completed`.
    pub(crate) async fn check_evaluation_done(&self, evaluation_id: EvaluationId) -> Result<()> {
        let active = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation_id))
            .filter(CBuild::Status.is_in(vec![
                BuildStatus::Created,
                BuildStatus::Queued,
                BuildStatus::Building,
                BuildStatus::FailedTransient,
            ]))
            .all(&self.state.worker_db)
            .await
            .context("fetch active builds")?;

        if !active.is_empty() {
            return Ok(());
        }

        let Some(eval) = EEvaluation::find_by_id(evaluation_id)
            .one(&self.state.worker_db)
            .await?
        else {
            return Ok(());
        };

        if !matches!(eval.status, EvaluationStatus::Building) {
            return Ok(());
        }

        let failed_builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation_id))
            .filter(CBuild::Status.is_in(vec![
                BuildStatus::FailedPermanent,
                BuildStatus::FailedTimeout,
                BuildStatus::DependencyFailed,
            ]))
            .all(&self.state.worker_db)
            .await
            .context("fetch failed builds")?;

        // Also treat error-level evaluation messages (nix eval errors, attr
        // resolution failures) as a failure signal - the evaluation was only
        // partially successful even if every discovered build passed.
        let eval_error_messages = EEvaluationMessage::find()
            .filter(CEvaluationMessage::Evaluation.eq(evaluation_id))
            .filter(CEvaluationMessage::Level.eq(gradient_entity::evaluation_message::MessageLevel::Error))
            .all(&self.state.worker_db)
            .await
            .context("fetch eval error messages")?;

        let target = if failed_builds.is_empty() && eval_error_messages.is_empty() {
            EvaluationStatus::Completed
        } else {
            EvaluationStatus::Failed
        };
        info!(
            %evaluation_id,
            ?target,
            failed_builds = failed_builds.len(),
            eval_errors = eval_error_messages.len(),
            "evaluation finished"
        );
        update_evaluation_status(&self.state.db(), eval, target).await;
        Ok(())
    }

    /// Sweep every in-flight evaluation and reconcile its status against the
    /// current set of connected workers.
    ///
    /// Two regimes apply, keyed on whether the eval has any pending builds:
    ///
    /// - **Pre-build phase** (no pending builds yet, status in
    ///   `Queued`/`Fetching`/`EvaluatingFlake`/`EvaluatingDerivation`): if no
    ///   worker is connected, flip to `Waiting` so the UI can explain why the
    ///   eval is stuck. A `Waiting` eval with no pending builds came from this
    ///   path; if a worker has since connected, recover to `Queued` and let the
    ///   dispatch loop replay the normal progression.
    /// - **Build phase** (pending builds present, status in
    ///   `Building`/`Waiting`): flip `Building ↔ Waiting` based on whether any
    ///   pending build's `(architecture, required_features)` is satisfiable by
    ///   the connected pool, persisting the structured reason for the UI.
    pub async fn reconcile_waiting_state(
        &self,
        worker_caps: &[(Vec<String>, Vec<String>)],
        eval_capable_workers: usize,
    ) -> Result<()> {
        let evals = EEvaluation::find()
            .filter(CEvaluation::Status.is_in(vec![
                EvaluationStatus::Queued,
                EvaluationStatus::Fetching,
                EvaluationStatus::EvaluatingFlake,
                EvaluationStatus::EvaluatingDerivation,
                EvaluationStatus::Building,
                EvaluationStatus::Waiting,
            ]))
            .all(&self.state.worker_db)
            .await
            .context("fetch in-flight evaluations")?;
        if evals.is_empty() {
            return Ok(());
        }

        for eval in evals {
            // Approval and no-cache parks are owned by webhook + cache-create
            // hooks. The reconciler must not unpark them just because workers
            // showed up.
            if eval.status == EvaluationStatus::Waiting
                && eval
                    .waiting_reason
                    .as_ref()
                    .and_then(WaitingReason::from_json)
                    .is_some_and(|r| !matches!(r, WaitingReason::Workers { .. }))
            {
                continue;
            }

            let pending_builds = EBuild::find()
                .filter(CBuild::Evaluation.eq(eval.id))
                .filter(CBuild::Status.is_in(vec![
                    BuildStatus::Created,
                    BuildStatus::Queued,
                    BuildStatus::Building,
                    BuildStatus::FailedTransient,
                ]))
                .all(&self.state.worker_db)
                .await
                .context("fetch pending builds")?;

            let (target, new_reason) = if pending_builds.is_empty() {
                match eval.status {
                    EvaluationStatus::Building => continue,
                    EvaluationStatus::Queued
                    | EvaluationStatus::Fetching
                    | EvaluationStatus::EvaluatingFlake
                    | EvaluationStatus::EvaluatingDerivation
                    | EvaluationStatus::Waiting => {
                        match decide_pre_build_target(eval.status, eval_capable_workers) {
                            Some(pair) => pair,
                            None => continue,
                        }
                    }
                    _ => continue,
                }
            } else {
                if !matches!(
                    eval.status,
                    EvaluationStatus::Building | EvaluationStatus::Waiting
                ) {
                    continue;
                }
                let checker = BuildabilityChecker::load(self.state, &pending_builds).await?;
                let target = if checker.any_buildable(&pending_builds, worker_caps) {
                    EvaluationStatus::Building
                } else {
                    EvaluationStatus::Waiting
                };
                let reason = if matches!(target, EvaluationStatus::Waiting) {
                    Some(checker.compute_waiting_reason(&pending_builds, worker_caps))
                } else {
                    None
                };
                (target, reason)
            };

            if eval.status != target {
                info!(
                    evaluation_id = %eval.id,
                    from = ?eval.status,
                    to = ?target,
                    pending = pending_builds.len(),
                    workers = worker_caps.len(),
                    eval_workers = eval_capable_workers,
                    "reconciling evaluation waiting state"
                );
            }

            persist_waiting_reason(
                self.state,
                eval.id,
                &eval.waiting_reason,
                new_reason.as_ref(),
            )
            .await;

            if eval.status != target {
                update_evaluation_status(&self.state.db(), eval, target).await;
            }
        }

        Ok(())
    }
}

/// Decides whether a pre-build evaluation needs to be reconciled.
///
/// Returns `Some((target, reason))` when the evaluation should transition or
/// have its waiting reason refreshed; `None` when it is actively progressing
/// and must be left alone.
///
/// `eval_capable_workers` is the count of connected workers whose negotiated
/// `GradientCapabilities` includes `eval`. Pre-build states cannot make
/// progress unless that count is non-zero - even a fleet of build-only
/// workers leaves the eval stuck in `Queued`. Active pre-build states
/// (`Fetching`, `EvaluatingFlake`, `EvaluatingDerivation`) are owned by an
/// eval worker; they only stall into `Waiting` when every eval-capable
/// worker has disconnected, and they must never be reset back to `Queued`.
fn decide_pre_build_target(
    current: EvaluationStatus,
    eval_capable_workers: usize,
) -> Option<(EvaluationStatus, Option<WaitingReason>)> {
    let stall = || {
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::Workers {
                unmet: Vec::new(),
                connected_workers: 0,
                available_architectures: Vec::new(),
            }),
        )
    };
    match (current, eval_capable_workers) {
        (EvaluationStatus::Waiting, 0) => Some(stall()),
        (EvaluationStatus::Waiting, _) => Some((EvaluationStatus::Queued, None)),
        (
            EvaluationStatus::Queued
            | EvaluationStatus::Fetching
            | EvaluationStatus::EvaluatingFlake
            | EvaluationStatus::EvaluatingDerivation,
            0,
        ) => Some(stall()),
        _ => None,
    }
}

/// Update `evaluation.waiting_reason` only when the value actually changes.
///
/// Avoids a row-level UPDATE every reconcile cycle when the unmet capabilities
/// haven't shifted, which keeps `updated_at` from churning on the row.
async fn persist_waiting_reason(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
    current: &Option<serde_json::Value>,
    new_reason: Option<&WaitingReason>,
) {
    let new_value = new_reason.map(|r| r.to_json());

    let unchanged = match (current, &new_value) {
        (None, None) => true,
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    if unchanged {
        return;
    }

    let res = EEvaluation::update_many()
        .col_expr(
            CEvaluation::WaitingReason,
            sea_orm::sea_query::Expr::value(new_value),
        )
        .filter(CEvaluation::Id.eq(evaluation_id))
        .exec(&state.worker_db)
        .await;

    if let Err(e) = res {
        warn!(error = %e, %evaluation_id, "failed to persist waiting_reason");
    }
}

// ---------------------------------------------------------------------------
// Public free-function API (thin wrappers around BuildStateHandler)
// ---------------------------------------------------------------------------

pub async fn handle_build_output(
    state: &Arc<ServerState>,
    job: &PendingBuildJob,
    build_id: BuildId,
    outputs: Vec<BuildOutput>,
    metrics: Option<BuildMetrics>,
    substituted: bool,
) -> Result<()> {
    BuildStateHandler::new(state)
        .handle_build_output(job, build_id, outputs, metrics, substituted)
        .await
}

pub async fn handle_build_job_completed(
    state: &Arc<ServerState>,
    build_id: BuildId,
) -> Result<()> {
    BuildStateHandler::new(state)
        .handle_build_job_completed(build_id)
        .await
}

pub async fn handle_build_job_failed(
    state: &Arc<ServerState>,
    build_id: BuildId,
    error: &str,
    kind: BuildFailureKind,
) -> Result<()> {
    BuildStateHandler::new(state)
        .handle_build_job_failed(build_id, error, kind)
        .await
}

pub(crate) async fn check_evaluation_done(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
) -> Result<()> {
    BuildStateHandler::new(state)
        .check_evaluation_done(evaluation_id)
        .await
}

pub async fn reconcile_waiting_state(
    state: &Arc<ServerState>,
    worker_caps: &[(Vec<String>, Vec<String>)],
    eval_capable_workers: usize,
) -> Result<()> {
    BuildStateHandler::new(state)
        .reconcile_waiting_state(worker_caps, eval_capable_workers)
        .await
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Pre-loaded derivation and feature data for a set of pending builds.
///
/// Used by [`BuildStateHandler::reconcile_waiting_state`] to determine whether
/// any pending build can be satisfied by the current worker pool without
/// re-querying the DB per evaluation.
struct BuildabilityChecker {
    drv_by_id: HashMap<DerivationId, MDerivation>,
    /// build_id → `SubstituteUnavailable` miss count. A substitutable build is
    /// only treated as buildable-anywhere while it is below the escalation
    /// threshold; past it, it is checked against real arch/features like any
    /// other build (so the parker can park it when no arch worker exists).
    substitute_misses: HashMap<BuildId, i64>,
    /// Maps derivation ID → list of required feature IDs.
    features_by_drv: HashMap<DerivationId, Vec<FeatureId>>,
    feature_name: HashMap<FeatureId, String>,
}

impl BuildabilityChecker {
    /// Query the DB for all derivations, required features, and substitute-miss
    /// counts referenced by `builds`, returning a checker ready to call
    /// [`any_buildable`].
    ///
    /// [`any_buildable`]: BuildabilityChecker::any_buildable
    async fn load(state: &Arc<ServerState>, builds: &[MBuild]) -> Result<Self> {
        let db = &state.worker_db;
        let drv_ids: Vec<DerivationId> = builds.iter().map(|b| b.derivation).collect();
        let build_ids: Vec<BuildId> = builds.iter().map(|b| b.id).collect();
        let substitute_misses = gradient_db::substitute_miss_counts(db, &build_ids)
            .await
            .context("fetch substitute-miss counts")?;

        let drvs = gradient_db::fetch_in_chunks(&drv_ids, |chunk| async move {
            EDerivation::find().filter(CDerivation::Id.is_in(chunk)).all(db).await
        })
        .await
        .context("fetch derivations for pending builds")?;
        let drv_by_id: HashMap<DerivationId, MDerivation> =
            drvs.into_iter().map(|d| (d.id, d)).collect();

        let edges = gradient_db::fetch_in_chunks(&drv_ids, |chunk| async move {
            EDerivationFeature::find()
                .filter(CDerivationFeature::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .context("fetch derivation_feature edges")?;
        let mut features_by_drv: HashMap<DerivationId, Vec<FeatureId>> = HashMap::new();
        for e in &edges {
            features_by_drv
                .entry(e.derivation)
                .or_default()
                .push(e.feature);
        }

        let feature_ids: Vec<FeatureId> = edges.iter().map(|e| e.feature).collect();
        let feature_rows = gradient_db::fetch_in_chunks(&feature_ids, |chunk| async move {
            EFeature::find().filter(CFeature::Id.is_in(chunk)).all(db).await
        })
        .await
        .context("fetch feature names")?;
        let feature_name: HashMap<FeatureId, String> =
            feature_rows.into_iter().map(|f| (f.id, f.name)).collect();

        Ok(Self {
            drv_by_id,
            substitute_misses,
            features_by_drv,
            feature_name,
        })
    }

    /// True while `build` should still be dispatched as a substitute
    /// (builtin / any-worker): it is substitutable and below the escalation
    /// threshold. Once it crosses the threshold it is checked against its real
    /// arch/features instead.
    fn in_substitute_mode(&self, build: &MBuild) -> bool {
        build.substitutable
            && self.substitute_misses.get(&build.id).copied().unwrap_or(0)
                < crate::dispatch::SUBSTITUTE_MISS_ESCALATION_THRESHOLD
    }

    /// Returns `true` if at least one build in `builds` can be satisfied by
    /// some worker in `worker_caps`:
    /// `(build.arch ∈ worker.architectures) ∧ (∀ required feature ∈ worker.system_features)`.
    fn any_buildable(&self, builds: &[MBuild], worker_caps: &[(Vec<String>, Vec<String>)]) -> bool {
        builds.iter().any(|b| {
            if self.in_substitute_mode(b) {
                return true;
            }
            let Some(drv) = self.drv_by_id.get(&b.derivation) else {
                return false;
            };
            let required: Vec<&str> = self.required_features_for(&b.derivation);
            worker_caps.iter().any(|(arch, feats)| {
                let arch_ok =
                    drv.architecture == "builtin" || arch.iter().any(|a| a == &drv.architecture);
                let feats_ok = required.iter().all(|f| feats.iter().any(|sf| sf == f));
                arch_ok && feats_ok
            })
        })
    }

    fn required_features_for(&self, drv_id: &DerivationId) -> Vec<&str> {
        self.features_by_drv
            .get(drv_id)
            .map(|ids| {
                let mut names: Vec<&str> = ids
                    .iter()
                    .filter_map(|i| self.feature_name.get(i).map(String::as_str))
                    .collect();
                names.sort_unstable();
                names.dedup();
                names
            })
            .unwrap_or_default()
    }

    /// Group every unsatisfiable `(architecture, required_features)` combo and
    /// the number of pending builds it covers. Used for the API
    /// `waiting_reason` payload so the UI can explain *why* nothing is
    /// dispatching.
    fn compute_waiting_reason(
        &self,
        builds: &[MBuild],
        worker_caps: &[(Vec<String>, Vec<String>)],
    ) -> WaitingReason {
        let mut grouped: BTreeMap<(String, Vec<String>), u32> = BTreeMap::new();
        for b in builds {
            if self.in_substitute_mode(b) {
                continue;
            }
            let Some(drv) = self.drv_by_id.get(&b.derivation) else {
                continue;
            };
            let required_owned: Vec<String> = self
                .required_features_for(&b.derivation)
                .into_iter()
                .map(str::to_owned)
                .collect();
            let satisfied = worker_caps.iter().any(|(arch, feats)| {
                let arch_ok =
                    drv.architecture == "builtin" || arch.iter().any(|a| a == &drv.architecture);
                let feats_ok = required_owned
                    .iter()
                    .all(|f| feats.iter().any(|sf| sf == f));
                arch_ok && feats_ok
            });
            if satisfied {
                continue;
            }
            *grouped
                .entry((drv.architecture.clone(), required_owned))
                .or_default() += 1;
        }

        let unmet: Vec<UnmetRequirement> = grouped
            .into_iter()
            .map(
                |((architecture, required_features), build_count)| UnmetRequirement {
                    architecture,
                    required_features,
                    build_count,
                },
            )
            .collect();

        let mut available_architectures: Vec<String> = worker_caps
            .iter()
            .flat_map(|(archs, _)| archs.iter().cloned())
            .collect();
        available_architectures.sort_unstable();
        available_architectures.dedup();

        WaitingReason::Workers {
            unmet,
            connected_workers: worker_caps.len() as u32,
            available_architectures,
        }
    }
}

#[cfg(test)]
mod retry_tests {
    use super::{FailureOutcome, decide_failure_outcome, retry_backoff_elapsed};
    use gradient_types::proto::BuildFailureKind;

    #[test]
    fn permanent_is_terminal_regardless_of_attempt() {
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Permanent, 0, 3),
            FailureOutcome::Permanent
        );
    }
    #[test]
    fn timeout_is_terminal() {
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Timeout, 0, 3),
            FailureOutcome::Timeout
        );
    }
    #[test]
    fn transient_retries_until_budget_then_permanent() {
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Transient, 0, 3),
            FailureOutcome::Retry
        );
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Transient, 1, 3),
            FailureOutcome::Retry
        );
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Transient, 2, 3),
            FailureOutcome::Permanent
        );
    }
    #[test]
    fn substitute_unavailable_requeues_penalty_free() {
        for attempt in [0, 5, 100] {
            assert_eq!(
                decide_failure_outcome(BuildFailureKind::SubstituteUnavailable, attempt, 3),
                FailureOutcome::Requeue
            );
        }
    }
    #[test]
    fn backoff_grows_per_attempt() {
        let t0 = chrono::NaiveDateTime::default();
        assert!(!retry_backoff_elapsed(1, t0, t0 + chrono::Duration::seconds(29), 30));
        assert!(retry_backoff_elapsed(1, t0, t0 + chrono::Duration::seconds(30), 30));
        assert!(!retry_backoff_elapsed(2, t0, t0 + chrono::Duration::seconds(59), 30));
        assert!(retry_backoff_elapsed(2, t0, t0 + chrono::Duration::seconds(60), 30));
    }
}

#[cfg(test)]
mod waiting_reason_tests {
    use super::*;

    fn workers_view(r: &WaitingReason) -> (&[UnmetRequirement], u32, &[String]) {
        match r {
            WaitingReason::Workers {
                unmet,
                connected_workers,
                available_architectures,
            } => (unmet, *connected_workers, available_architectures),
            other => panic!("expected Workers variant, got {other:?}"),
        }
    }

    fn drv(id: DerivationId, arch: &str) -> MDerivation {
        gradient_entity::derivation::Model {
            id,
            organization: OrganizationId::nil(),
            hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            name: "x".into(),
            architecture: arch.into(),
            created_at: chrono::NaiveDateTime::default(),
            ..Default::default()
        }
    }

    fn build_for(drv_id: DerivationId, eval_id: EvaluationId) -> MBuild {
        gradient_entity::build::Model {
            id: BuildId::now_v7(),
            evaluation: eval_id,
            derivation: drv_id,
            status: BuildStatus::Queued,
            ..Default::default()
        }
    }

    fn checker_with(
        drvs: Vec<MDerivation>,
        feature_edges: Vec<(DerivationId, FeatureId, &'static str)>,
    ) -> BuildabilityChecker {
        let drv_by_id = drvs.into_iter().map(|d| (d.id, d)).collect();
        let mut features_by_drv: HashMap<DerivationId, Vec<FeatureId>> = HashMap::new();
        let mut feature_name: HashMap<FeatureId, String> = HashMap::new();
        for (drv_id, feat_id, name) in feature_edges {
            features_by_drv.entry(drv_id).or_default().push(feat_id);
            feature_name.insert(feat_id, name.to_string());
        }
        BuildabilityChecker {
            drv_by_id,
            substitute_misses: HashMap::new(),
            features_by_drv,
            feature_name,
        }
    }

    #[test]
    fn no_workers_lists_every_unique_arch() {
        let eval_id = EvaluationId::now_v7();
        let d1 = drv(DerivationId::now_v7(), "aarch64-linux");
        let d2 = drv(DerivationId::now_v7(), "x86_64-linux");
        let builds = vec![build_for(d1.id, eval_id), build_for(d2.id, eval_id)];
        let checker = checker_with(vec![d1, d2], vec![]);

        let reason = checker.compute_waiting_reason(&builds, &[]);
        let (unmet, connected_workers, available_architectures) = workers_view(&reason);

        assert_eq!(connected_workers, 0);
        assert!(available_architectures.is_empty());
        assert_eq!(unmet.len(), 2);
        assert!(
            unmet
                .iter()
                .any(|u| u.architecture == "aarch64-linux" && u.build_count == 1)
        );
        assert!(
            unmet
                .iter()
                .any(|u| u.architecture == "x86_64-linux" && u.build_count == 1)
        );
    }

    #[test]
    fn satisfied_builds_are_excluded_from_unmet() {
        let eval_id = EvaluationId::now_v7();
        let d_x86 = drv(DerivationId::now_v7(), "x86_64-linux");
        let d_arm = drv(DerivationId::now_v7(), "aarch64-linux");
        let builds = vec![build_for(d_x86.id, eval_id), build_for(d_arm.id, eval_id)];
        let checker = checker_with(vec![d_x86, d_arm], vec![]);

        let caps: Vec<(Vec<String>, Vec<String>)> = vec![(vec!["x86_64-linux".into()], vec![])];
        let reason = checker.compute_waiting_reason(&builds, &caps);
        let (unmet, connected_workers, available_architectures) = workers_view(&reason);

        assert_eq!(connected_workers, 1);
        assert_eq!(available_architectures, ["x86_64-linux"]);
        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].architecture, "aarch64-linux");
        assert_eq!(unmet[0].build_count, 1);
    }

    #[test]
    fn missing_feature_is_reported_alongside_arch() {
        let eval_id = EvaluationId::now_v7();
        let drv_id = DerivationId::now_v7();
        let feat_id = FeatureId::now_v7();
        let d = drv(drv_id, "x86_64-linux");
        let builds = vec![build_for(drv_id, eval_id)];
        let checker = checker_with(vec![d], vec![(drv_id, feat_id, "kvm")]);

        let caps: Vec<(Vec<String>, Vec<String>)> = vec![(vec!["x86_64-linux".into()], vec![])];
        let reason = checker.compute_waiting_reason(&builds, &caps);
        let (unmet, _, _) = workers_view(&reason);

        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].architecture, "x86_64-linux");
        assert_eq!(unmet[0].required_features, vec!["kvm".to_string()]);
        assert_eq!(unmet[0].build_count, 1);
    }

    #[test]
    fn identical_requirements_are_grouped_with_count() {
        let eval_id = EvaluationId::now_v7();
        let d1 = drv(DerivationId::now_v7(), "aarch64-linux");
        let d2 = drv(DerivationId::now_v7(), "aarch64-linux");
        let d3 = drv(DerivationId::now_v7(), "aarch64-linux");
        let builds = vec![
            build_for(d1.id, eval_id),
            build_for(d2.id, eval_id),
            build_for(d3.id, eval_id),
        ];
        let checker = checker_with(vec![d1, d2, d3], vec![]);

        let reason = checker.compute_waiting_reason(&builds, &[]);
        let (unmet, _, _) = workers_view(&reason);

        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].architecture, "aarch64-linux");
        assert_eq!(unmet[0].build_count, 3);
    }

    #[test]
    fn pre_build_target_queued_no_workers_stalls_to_waiting() {
        let (target, reason) = decide_pre_build_target(EvaluationStatus::Queued, 0)
            .expect("stall must produce a transition");
        assert_eq!(target, EvaluationStatus::Waiting);
        let reason = reason.expect("stall target must carry a reason");
        let (unmet, connected_workers, available_architectures) = workers_view(&reason);
        assert_eq!(connected_workers, 0);
        assert!(unmet.is_empty());
        assert!(available_architectures.is_empty());
    }

    #[test]
    fn pre_build_target_waiting_with_workers_recovers_to_queued() {
        let (target, reason) = decide_pre_build_target(EvaluationStatus::Waiting, 2)
            .expect("recovery must produce a transition");
        assert_eq!(target, EvaluationStatus::Queued);
        assert!(reason.is_none());
    }

    #[test]
    fn pre_build_target_waiting_no_workers_keeps_waiting() {
        let (target, reason) = decide_pre_build_target(EvaluationStatus::Waiting, 0)
            .expect("waiting must refresh its reason");
        assert_eq!(target, EvaluationStatus::Waiting);
        assert!(reason.is_some());
    }

    #[test]
    fn pre_build_target_queued_with_workers_is_noop() {
        assert!(decide_pre_build_target(EvaluationStatus::Queued, 1).is_none());
    }

    #[test]
    fn pre_build_target_active_pre_build_with_workers_left_alone() {
        // Regression: a Fetching/EvaluatingFlake/EvaluatingDerivation eval is
        // already being processed by an eval worker. Reconcile must not push
        // it back to Queued - that violates the state machine and would log a
        // spurious "invalid status transition: Fetching → Queued" warning.
        for status in [
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
        ] {
            assert!(
                decide_pre_build_target(status, 1).is_none(),
                "{status:?} with workers connected must not be reconciled"
            );
        }
    }

    #[test]
    fn pre_build_target_active_pre_build_no_workers_stalls() {
        for status in [
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
        ] {
            let (target, reason) = decide_pre_build_target(status, 0)
                .unwrap_or_else(|| panic!("{status:?} with no workers must stall"));
            assert_eq!(target, EvaluationStatus::Waiting);
            assert!(reason.is_some());
        }
    }

    /// Regression for issue #268: a Queued evaluation in an org whose only
    /// connected workers lack the `eval` capability must stall to Waiting.
    /// The caller in `BuildStateHandler::reconcile_waiting_state` passes the
    /// eval-capable count - not total connected workers - so the function
    /// sees `0` here and produces the same `Workers { connected_workers: 0 }`
    /// reason as the no-workers-at-all path.
    #[test]
    fn pre_build_target_queued_no_eval_capable_workers_stalls() {
        let (target, reason) = decide_pre_build_target(EvaluationStatus::Queued, 0)
            .expect("stall must produce a transition");
        assert_eq!(target, EvaluationStatus::Waiting);
        let reason = reason.expect("stall target must carry a reason");
        let (_, connected_workers, _) = workers_view(&reason);
        assert_eq!(connected_workers, 0);
    }

    #[test]
    fn builtin_arch_satisfied_by_any_worker() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "builtin");
        let builds = vec![build_for(d.id, eval_id)];
        let checker = checker_with(vec![d], vec![]);

        let caps: Vec<(Vec<String>, Vec<String>)> = vec![(vec!["x86_64-linux".into()], vec![])];
        let reason = checker.compute_waiting_reason(&builds, &caps);
        let (unmet, _, _) = workers_view(&reason);

        assert!(unmet.is_empty());
    }

    fn substitutable_build(drv_id: DerivationId, eval_id: EvaluationId) -> MBuild {
        gradient_entity::build::Model {
            id: BuildId::now_v7(),
            evaluation: eval_id,
            derivation: drv_id,
            status: BuildStatus::Queued,
            substitutable: true,
            ..Default::default()
        }
    }

    #[test]
    fn substitutable_below_threshold_is_buildable_anywhere() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "aarch64-linux");
        let build = substitutable_build(d.id, eval_id);
        let mut checker = checker_with(vec![d], vec![]);
        checker
            .substitute_misses
            .insert(build.id, crate::dispatch::SUBSTITUTE_MISS_ESCALATION_THRESHOLD - 1);

        let caps: Vec<(Vec<String>, Vec<String>)> = vec![(vec!["x86_64-linux".into()], vec![])];
        let builds = [build];
        assert!(checker.any_buildable(&builds, &caps));
        let reason = checker.compute_waiting_reason(&builds, &caps);
        let (unmet, _, _) = workers_view(&reason);
        assert!(unmet.is_empty());
    }

    #[test]
    fn substitutable_at_threshold_escalates_to_real_arch_check() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "aarch64-linux");
        let build = substitutable_build(d.id, eval_id);
        let mut checker = checker_with(vec![d], vec![]);
        checker
            .substitute_misses
            .insert(build.id, crate::dispatch::SUBSTITUTE_MISS_ESCALATION_THRESHOLD);

        // No aarch64 worker: the escalated build is no longer buildable-anywhere
        // and surfaces as an unmet aarch64 requirement so the parker can park it.
        let caps: Vec<(Vec<String>, Vec<String>)> = vec![(vec!["x86_64-linux".into()], vec![])];
        let builds = [build];
        assert!(!checker.any_buildable(&builds, &caps));
        let reason = checker.compute_waiting_reason(&builds, &caps);
        let (unmet, _, _) = workers_view(&reason);
        assert_eq!(unmet.len(), 1);
        assert_eq!(unmet[0].architecture, "aarch64-linux");
    }
}
