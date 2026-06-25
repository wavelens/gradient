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
    cascade_dependency_failed, eval_anchor_statuses, fail_latest_attempt,
    update_derivation_build_status, update_evaluation_status,
};
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
};
use tracing::{error, info, warn};

use super::jobs::{PendingBuildJob, PendingJob};
use crate::dispatch_mode::{arch_available, decide_dispatch_mode, BuildDispatchMode};
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
        // The build itself is terminal for this eval; the self-heal that
        // re-queues the missing inputs' producers runs separately.
        BuildFailureKind::InputsUnavailable => FailureOutcome::Permanent,
        BuildFailureKind::Transient => {
            if (attempt + 1) < max_attempts as i32 {
                FailureOutcome::Retry
            } else {
                FailureOutcome::Permanent
            }
        }
    }
}

/// Terminal success status for a build whose job completed. `Substituted` when
/// the daemon found the outputs already valid and ran no build (recorded on
/// `build.substituted`), else `Completed`. Decided at `JobCompleted`, after the
/// worker has pushed the output NARs, so a build never reaches a dispatch-ready
/// terminal state while its bytes are still absent from the cache - the #399
/// regression where a dependent dispatched into that window and failed
/// `InputsUnavailable`.
pub(crate) fn terminal_success_status(outputs_already_valid: bool) -> BuildStatus {
    if outputs_already_valid {
        BuildStatus::Substituted
    } else {
        BuildStatus::Completed
    }
}

/// Best-effort mapping from the worker's failure classification to a stored
/// `build_attempt.reason`. `Transient` has no single cause, so it stays `None`.
fn attempt_reason(kind: BuildFailureKind) -> Option<AttemptFailureReason> {
    match kind {
        BuildFailureKind::SubstituteUnavailable | BuildFailureKind::InputsUnavailable => {
            Some(AttemptFailureReason::SubstituteUnavailable)
        }
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

/// Extract the 32-char hash from a `/nix/store/<hash>-<name>` store path.
fn store_path_hash(store_path: &str) -> Option<&str> {
    store_path
        .strip_prefix("/nix/store/")
        .and_then(|s| s.split('-').next())
        .filter(|h| !h.is_empty())
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
        derivation_build: DerivationBuildId,
        outputs: Vec<BuildOutput>,
        metrics: Option<BuildMetrics>,
        substituted: bool,
    ) -> Result<()> {
        let anchor = EDerivationBuild::find_by_id(derivation_build)
            .one(&self.state.worker_db)
            .await
            .context("fetch derivation_build")?
            .with_context(|| format!("derivation_build {} not found", derivation_build))?;

        let build_id = anchor.id;
        let derivation_id = anchor.derivation;

        // Per-build metrics: a multi-build job yields one `BuildOutput` per
        // build, so this records exactly one `derivation_metric` row per anchor.
        if let Some(metrics) = metrics {
            self.record_metrics(&anchor, derivation_id, &metrics)
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
                    let am = MBuildProduct {
                        id: BuildProductId::now_v7(),
                        derivation_output: row_id,
                        file_type: product.file_type.clone(),
                        subtype: product.subtype.clone(),
                        name: product.name.clone(),
                        path: product.path.clone(),
                        size: product.size.map(|s| s as i64),
                        created_at: gradient_types::now(),
                    }
                    .into_active_model();

                    if let Err(e) = am.insert(&self.state.worker_db).await {
                        warn!(error = %e, %build_id, output_name = %output.name, "failed to insert build_product");
                    }
                }
            } else {
                warn!(%build_id, output_name = %output.name, "derivation_output row not found");
            }
        }

        info!(%build_id, output_count = outputs.len(), "build outputs recorded");

        // The daemon found the outputs already valid - no build ran. Record that
        // on the anchor but do NOT move it terminal here: the worker has not yet
        // pushed the output NARs (`compress_and_push_paths` runs at end of the
        // job, before `JobCompleted`). Flipping to `Substituted` now would make
        // the anchor dispatch-ready while its bytes are still absent from the
        // cache, so a dependent dispatched into that window fails
        // `InputsUnavailable` (#399). `handle_build_job_completed` finalizes the
        // terminal status from this flag, after the push (#303).
        if substituted {
            let mut active = anchor.into_active_model();
            active.substituted = Set(true);
            active.updated_at = Set(gradient_types::now());
            if let Err(e) = active.update(&self.state.worker_db).await {
                warn!(%build_id, error = %e, "failed to record anchor as substituted");
            }
        }

        Ok(())
    }

    pub async fn handle_build_job_completed(&self, derivation_build: DerivationBuildId) -> Result<()> {
        let anchor = match EDerivationBuild::find_by_id(derivation_build)
            .one(&self.state.worker_db)
            .await?
        {
            Some(a) => a,
            None => {
                warn!(%derivation_build, "anchor not found on job_completed");
                return Ok(());
            }
        };
        let derivation_id = anchor.derivation;
        let was_external_cached = anchor.substitutable;

        // The worker has finished pushing this job's output NARs by the time
        // `JobCompleted` arrives, so it is now safe to make the anchor
        // dispatch-ready. `Substituted` when the daemon found the outputs
        // already valid (recorded on the anchor by `handle_build_output`), else
        // `Completed`. `update_derivation_build_status` finalizes the
        // closure-complete flag, promotes dependents, and fans the reactor/
        // eval-done signal across referencing evals.
        let terminal = terminal_success_status(anchor.substituted);
        update_derivation_build_status(&self.state.db(), anchor, terminal).await;

        if was_external_cached {
            let state = Arc::clone(self.state);
            tokio::spawn(async move {
                let drv_path = match EDerivation::find_by_id(derivation_id)
                    .one(&state.worker_db)
                    .await
                {
                    Ok(Some(d)) => d.drv_path(),
                    Ok(None) => {
                        warn!(%derivation_build, %derivation_id, "substitute_log: derivation row missing");
                        return;
                    }
                    Err(e) => {
                        warn!(%derivation_build, error = %e, "substitute_log: derivation lookup failed");
                        return;
                    }
                };
                if let Err(e) = crate::log_substitution::substitute_log(
                    state,
                    derivation_build,
                    derivation_id,
                    drv_path,
                    true,
                )
                .await
                {
                    warn!(%derivation_build, error = %e, "substitute_log spawn failed");
                }
            });
        }

        self.check_referencing_evals_done(derivation_id).await
    }

    /// Insert a `derivation_metric` history row from a build's worker metrics.
    /// Called once per build from the `BuildOutput` handler.
    async fn record_metrics(
        &self,
        anchor: &MDerivationBuild,
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

        let metric = MDerivationMetric {
            id: DerivationMetricId::now_v7(),
            derivation: derivation_id,
            pname,
            closure_size,
            peak_ram_mb: metrics.peak_ram_mb.map(|v| v as i64),
            cpu_time_ms: metrics.cpu_time_ms.map(|v| v as i64),
            avg_cpu_pct: metrics.avg_cpu_pct.map(|v| v as f64),
            disk_read_bytes: metrics.disk_read_bytes.map(|v| v as i64),
            disk_write_bytes: metrics.disk_write_bytes.map(|v| v as i64),
            peak_network_mbps: metrics.peak_network_mbps.map(|v| v as f64),
            oom_killed: metrics.oom_killed,
            build_time_ms: metrics.build_time_ms.map(|v| v as i64),
            worker_id: gradient_db::latest_attempt_worker(&self.state.worker_db, anchor.id)
                .await
                .ok()
                .flatten()
                .unwrap_or_default(),
            created_at: gradient_types::now(),
        }
        .into_active_model();

        if let Err(e) = metric.insert(&self.state.worker_db).await {
            warn!(%derivation_id, error = %e, "failed to record derivation_metric");
        }
    }

    pub async fn handle_build_job_failed(
        &self,
        derivation_build: DerivationBuildId,
        error: &str,
        kind: BuildFailureKind,
        missing_paths: &[String],
    ) -> Result<()> {
        let anchor = match EDerivationBuild::find_by_id(derivation_build)
            .one(&self.state.worker_db)
            .await?
        {
            Some(a) => a,
            None => {
                warn!(%derivation_build, "anchor not found on job_failed");
                return Ok(());
            }
        };

        // Surface the worker's failure reason in the build log so the
        // frontend's log viewer renders it. Without this, pre-`nix build`
        // aborts (prefetch-time errors, daemon connection failures, etc.)
        // produce a Failed badge with an empty log - useless for diagnosis.
        if let Some(attempt_id) =
            gradient_db::latest_attempt_id(&self.state.worker_db, anchor.id)
                .await
                .ok()
                .flatten()
            && let Err(e) = self
                .state
                .log_storage
                .append(attempt_id, &format!("\n=== build failed: {error} ===\n"))
                .await
        {
            warn!(%derivation_build, error = %e, "failed to append worker error to build log");
        }

        let derivation_id = anchor.derivation;
        let attempt = anchor.attempt;
        let max_attempts = self.state.config.eval.build_max_attempts;

        if let Err(e) = fail_latest_attempt(
            &self.state.worker_db,
            derivation_build,
            AttemptOutcome::Failed,
            attempt_reason(kind),
        )
        .await
        {
            warn!(%derivation_build, error = %e, "failed to record attempt failure reason");
        }

        // Self-heal: a required input was reported absent from the cache while
        // its producer was marked done/substituted. Purge those stale outputs so
        // the next evaluation rebuilds them; this anchor stays terminal here.
        if matches!(kind, BuildFailureKind::InputsUnavailable)
            && !missing_paths.is_empty()
            && let Err(e) = self
                .reconcile_missing_inputs(derivation_id, missing_paths)
                .await
        {
            warn!(%derivation_build, error = %e, "failed to reconcile missing inputs");
        }

        match decide_failure_outcome(kind, attempt, max_attempts) {
            FailureOutcome::Retry => {
                let mut active: ADerivationBuild = anchor.clone().into_active_model();
                active.attempt = Set(attempt + 1);
                if let Err(e) = active.update(&self.state.worker_db).await {
                    error!(%derivation_build, error = %e, "failed to bump anchor attempt");
                }
                let reloaded = EDerivationBuild::find_by_id(derivation_build)
                    .one(&self.state.worker_db)
                    .await?
                    .unwrap_or(anchor);
                update_derivation_build_status(&self.state.db(), reloaded, BuildStatus::FailedTransient)
                    .await;
                info!(%derivation_build, attempt = attempt + 1, "transient build failure; scheduled for retry");
                return Ok(());
            }
            FailureOutcome::Requeue => {
                // Substitute miss: back to the queue without an `attempt` bump
                // or a permanent mark. Dispatch escalates to a real build once
                // the substitute-miss count crosses the threshold. Dependents are
                // untouched - nothing failed.
                update_derivation_build_status(&self.state.db(), anchor, BuildStatus::Queued).await;
                info!(%derivation_build, "substitute unavailable; re-queued for re-dispatch/escalation");
                return Ok(());
            }
            FailureOutcome::Permanent => {
                update_derivation_build_status(&self.state.db(), anchor, BuildStatus::FailedPermanent)
                    .await;
            }
            FailureOutcome::Timeout => {
                update_derivation_build_status(&self.state.db(), anchor, BuildStatus::FailedTimeout)
                    .await;
            }
        }

        // `update_derivation_build_status` already cascades dependency failure on
        // a terminal-failure transition; this is a belt-and-braces global cascade
        // over the same graph, idempotent on already-failed anchors.
        cascade_dependency_failed(&self.state.worker_db, derivation_id).await?;
        self.check_referencing_evals_done(derivation_id).await
    }

    /// Self-heal for `BuildFailureKind::InputsUnavailable`. A build attempt
    /// proved these input paths are unfetchable from the cache, so purge each
    /// one's stale cache artifact (delete the `cached_path` row + the NAR object,
    /// clear the output's `is_cached` / `cached_path`) while leaving the
    /// derivation graph intact. The next evaluation then sees the path as never
    /// cached and rebuilds or re-fetches it. This build is already terminal for
    /// this eval (`InputsUnavailable` is permanent), so we don't re-queue
    /// anything here - the rebuild belongs to the next evaluation.
    ///
    /// A missing input with no producing derivation (a `.drv` file or a source
    /// path) has its stale row + object purged just the same; the next eval
    /// re-instantiates the `.drv` and re-pushes it, so this is a recoverable
    /// purge, not a dead-end.
    async fn reconcile_missing_inputs(
        &self,
        failed_derivation: DerivationId,
        missing_paths: &[String],
    ) -> Result<()> {
        let db = &self.state.worker_db;
        let mut purged = 0usize;
        let mut referrers_demoted = 0usize;
        let mut sources_purged: Vec<&str> = Vec::new();
        let mut demoted_producers: Vec<DerivationId> = Vec::new();
        for path in missing_paths {
            let Some(hash) = store_path_hash(path) else {
                continue;
            };

            // Diagnostic: record why the worker found this input unfetchable
            // even though dispatch treated its producer as done. `fully_cached`
            // true means the DB claimed a complete NAR (stale cached_path / lost
            // object); the producer statuses show whether it was trusted
            // `Substituted` or really `Completed`. The eval arg is unused by the
            // global diagnosis query.
            match gradient_db::diagnose_missing_input(db, EvaluationId::now_v7(), hash).await {
                Ok(d) => warn!(
                    %path,
                    hash,
                    cached_path_present = d.cached_path_present,
                    fully_cached = d.fully_cached,
                    outputs_cached = d.outputs_cached,
                    outputs_total = d.outputs_total,
                    producer_statuses = ?d.producer_build_statuses,
                    "missing input: cache/build state at failure"
                ),
                Err(e) => warn!(%path, error = %e, "missing input: diagnosis query failed"),
            }

            match gradient_db::demote_cached_output(db, &self.state.nar_storage, hash).await {
                Ok(drvs) if !drvs.is_empty() => {
                    purged += 1;
                    demoted_producers.extend(drvs);
                    // The leaf rebuilds + re-pushes closure-complete; meanwhile drop
                    // the now-stale `closure_complete` up the chain so the dispatch
                    // gate re-blocks dependents until the closure is whole again.
                    if let Err(e) =
                        gradient_db::clear_closure_complete_for_referrers(db, hash).await
                    {
                        warn!(%path, error = %e, "reconcile: clear closure_complete failed");
                    }
                }
                Ok(_) => {
                    sources_purged.push(path);
                    // No producing derivation (a source / `.drv`): it only returns
                    // to the cache as part of a referrer's closure, so demote the
                    // referrers - a pruned/substituted parent would otherwise never
                    // re-push it, stranding dependents forever.
                    match gradient_db::demote_referrers_of(db, &self.state.nar_storage, hash).await {
                        Ok(drvs) => {
                            referrers_demoted += drvs.len();
                            demoted_producers.extend(drvs);
                        }
                        Err(e) => warn!(%path, error = %e, "reconcile: demote referrers failed"),
                    }
                }
                Err(e) => warn!(%path, error = %e, "reconcile: purge cached output failed"),
            }
        }

        // A demanded output whose producer is terminal-*failed* (not 3/7, which
        // `demote_cached_output` already reset) must retry: the dependent that just
        // failed is a fresh build intent, and waiting for an eval to requeue it
        // dead-ends whenever evals are aborted. Re-queue on demand, decoupled from
        // eval completion - `requeue_failed_anchors` only touches statuses 4/5/6/9.
        let requeued = if demoted_producers.is_empty() {
            0
        } else {
            gradient_db::requeue_failed_anchors(db, &demoted_producers)
                .await
                .unwrap_or_else(|e| {
                    warn!(error = %e, "reconcile: requeue failed producers failed");
                    0
                })
        };

        if !sources_purged.is_empty() {
            info!(
                %failed_derivation,
                count = sources_purged.len(),
                sample = ?sources_purged.iter().take(5).collect::<Vec<_>>(),
                "reconcile: purged stale cache rows + objects for inputs with no producing \
                 derivation (.drv / source); the next evaluation re-instantiates and re-pushes them"
            );
        }

        info!(
            %failed_derivation,
            purged,
            sources_purged = sources_purged.len(),
            referrers_demoted,
            requeued,
            paths = missing_paths.len(),
            "reconciled missing inputs; stale cache rows + objects purged for next-eval rebuild"
        );
        Ok(())
    }

    /// After an anchor reaches a terminal status, sweep every evaluation that
    /// references the derivation (via `build_job`) and finalize the ones whose
    /// whole anchor set is now resolved.
    async fn check_referencing_evals_done(&self, derivation: DerivationId) -> Result<()> {
        let evals =
            gradient_db::evals_referencing_derivation(&self.state.worker_db, derivation).await?;
        for evaluation_id in evals {
            self.check_evaluation_done(evaluation_id).await?;
        }

        Ok(())
    }

    /// Transitions the evaluation to its final state if all its anchors are done.
    ///
    /// Graph-derived via `eval_anchor_statuses`: returns early while any anchor is
    /// still active (Created/Queued/Building/FailedTransient) or the evaluation is
    /// not `Building`. Otherwise `Failed` when any anchor is a terminal failure or
    /// the eval has an error message, else `Completed`.
    pub(crate) async fn check_evaluation_done(&self, evaluation_id: EvaluationId) -> Result<()> {
        let statuses = eval_anchor_statuses(&self.state.worker_db, evaluation_id)
            .await
            .context("fetch eval anchor statuses")?;

        let any_active = statuses.iter().any(|s| {
            matches!(
                s,
                BuildStatus::Created
                    | BuildStatus::Queued
                    | BuildStatus::Building
                    | BuildStatus::FailedTransient
            )
        });
        if any_active {
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

        let any_failed = statuses.iter().any(|s| {
            matches!(
                s,
                BuildStatus::FailedPermanent
                    | BuildStatus::FailedTimeout
                    | BuildStatus::DependencyFailed
            )
        });

        // Also treat error-level evaluation messages (nix eval errors, attr
        // resolution failures) as a failure signal - the evaluation was only
        // partially successful even if every discovered build passed.
        let eval_error_messages = EEvaluationMessage::find()
            .filter(CEvaluationMessage::Evaluation.eq(evaluation_id))
            .filter(CEvaluationMessage::Level.eq(gradient_entity::evaluation_message::MessageLevel::Error))
            .all(&self.state.worker_db)
            .await
            .context("fetch eval error messages")?;

        let target = if !any_failed && eval_error_messages.is_empty() {
            EvaluationStatus::Completed
        } else {
            EvaluationStatus::Failed
        };
        info!(
            %evaluation_id,
            ?target,
            any_failed,
            eval_errors = eval_error_messages.len(),
            "evaluation finished"
        );

        // Authoritative resync of the entry-point histogram now the eval has
        // settled. The incremental deltas (#383) fire only from the single-row
        // status hook; every bulk path (promote_ready/promote_dependents/
        // cascade_dependency_failed/requeue_failed_anchors) moves anchors with
        // raw SQL that bypasses it, so the live counts drift. A terminal eval has
        // a fixed graph, so one recompute makes the displayed bar exact.
        if let Err(e) =
            gradient_db::reconcile_eval_dep_counts(&self.state.worker_db, evaluation_id).await
        {
            warn!(error = %e, %evaluation_id, "reconcile_eval_dep_counts at eval settle failed");
        }

        update_evaluation_status(&self.state.db(), eval, target).await;
        Ok(())
    }

    /// Sweep every in-flight evaluation and reconcile its status against the
    /// current set of connected workers, keyed on the eval's current state:
    ///
    /// - **Pre-build** (`Queued`/`Fetching`/`EvaluatingFlake`/
    ///   `EvaluatingDerivation`): park to `Waiting` with an `EvalWorkers` reason
    ///   when no worker provides the capability the state needs - `fetch` for
    ///   `Fetching`, `eval` otherwise - regardless of any builds the eval has
    ///   already batched (so a mid-eval stall is caught, not skipped).
    /// - **Build** (`Building`): flip `Building ↔ Waiting` from whether the pool
    ///   can satisfy any pending build's `(architecture, required_features)`.
    /// - **Waiting**: recover via the reason it parked under - `EvalWorkers`
    ///   back to `Queued` once the capability returns, `Workers` back to
    ///   `Building` once buildable. `Approval`/`NoCache`/`CacheStorageFull` parks
    ///   are owned by other hooks and left untouched.
    pub async fn reconcile_waiting_state(
        &self,
        worker_caps: &[(Vec<String>, Vec<String>)],
        eval_capable_workers: usize,
        fetch_capable_workers: usize,
        draining: bool,
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

        // Draining: park every in-flight evaluation so the server can be stopped
        // safely. Dispatch is already gated, so parked evals stay put until
        // draining is disabled (recovered to `Queued` by the branch below).
        if draining {
            for eval in evals {
                let reason = eval.waiting_reason.as_ref().and_then(WaitingReason::from_json);
                if eval.status == EvaluationStatus::Waiting
                    && matches!(
                        reason,
                        Some(WaitingReason::Draining) | Some(WaitingReason::Aborting)
                    )
                {
                    continue;
                }

                let needs_status_change = eval.status != EvaluationStatus::Waiting;
                persist_waiting_reason(
                    self.state,
                    eval.id,
                    &eval.waiting_reason,
                    Some(&WaitingReason::Draining),
                )
                .await;

                if needs_status_change {
                    info!(evaluation_id = %eval.id, from = ?eval.status, "parking evaluation: instance draining");
                    update_evaluation_status(&self.state.db(), eval, EvaluationStatus::Waiting).await;
                }
            }

            return Ok(());
        }

        let connected_workers = worker_caps.len() as u32;

        for eval in evals {
            let reason = eval.waiting_reason.as_ref().and_then(WaitingReason::from_json);

            // Approval, no-cache and storage-full parks are owned by webhook +
            // cache hooks; an Aborting park is owned by the abort path. The
            // reconciler must not unpark any of them just because workers showed up.
            if eval.status == EvaluationStatus::Waiting
                && reason.as_ref().is_some_and(|r| {
                    matches!(
                        r,
                        WaitingReason::Approval { .. }
                            | WaitingReason::NoCache
                            | WaitingReason::CacheStorageFull
                            | WaitingReason::Aborting
                    )
                })
            {
                continue;
            }

            let outcome = match eval.status {
                EvaluationStatus::Waiting => match reason {
                    Some(WaitingReason::EvalWorkers { capability, .. }) => Some(decide_eval_recovery(
                        capability,
                        eval_capable_workers,
                        fetch_capable_workers,
                        connected_workers,
                    )),
                    // Draining was disabled: resume from the queue and let the
                    // next pass re-park to the appropriate capacity reason.
                    Some(WaitingReason::Draining) => Some((EvaluationStatus::Queued, None)),
                    // Build-phase park, or a legacy/untagged row: recover from the
                    // pending builds, falling back to eval recovery when the eval
                    // never produced any (a pre-build park predating EvalWorkers).
                    _ => match self.build_phase_decision(eval.id, worker_caps).await? {
                        Some(pair) => Some(pair),
                        None => Some(decide_eval_recovery(
                            EvalCapability::Eval,
                            eval_capable_workers,
                            fetch_capable_workers,
                            connected_workers,
                        )),
                    },
                },
                EvaluationStatus::Queued
                | EvaluationStatus::Fetching
                | EvaluationStatus::EvaluatingFlake
                | EvaluationStatus::EvaluatingDerivation => decide_pre_build_target(
                    eval.status,
                    eval_capable_workers,
                    fetch_capable_workers,
                    connected_workers,
                ),
                EvaluationStatus::Building => {
                    self.build_phase_decision(eval.id, worker_caps).await?
                }
                _ => None,
            };

            let Some((target, new_reason)) = outcome else {
                continue;
            };

            if eval.status != target {
                info!(
                    evaluation_id = %eval.id,
                    from = ?eval.status,
                    to = ?target,
                    workers = connected_workers,
                    eval_workers = eval_capable_workers,
                    fetch_workers = fetch_capable_workers,
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

    /// Build-phase reconciliation for one evaluation: decide `Building` vs
    /// `Waiting` from whether the connected pool can satisfy any of the eval's
    /// pending anchors. Returns `None` when the eval has no pending anchor
    /// (nothing to decide).
    ///
    /// A `Waiting` verdict with an empty `unmet` set means the pool *can* build
    /// every pending anchor yet none is dispatchable - the whole set is `Created`,
    /// blocked behind the `closure_complete` gate with no in-flight build to fire
    /// a promotion. `propagate_closure_complete` can't reach this (it runs on
    /// completion events), so we self-heal here: [`attempt_graph_unstick`].
    ///
    /// [`attempt_graph_unstick`]: Self::attempt_graph_unstick
    async fn build_phase_decision(
        &self,
        evaluation_id: EvaluationId,
        worker_caps: &[(Vec<String>, Vec<String>)],
    ) -> Result<Option<(EvaluationStatus, Option<WaitingReason>)>> {
        let outcome = self.assess_buildability(evaluation_id, worker_caps).await?;

        if let Some((EvaluationStatus::Waiting, Some(WaitingReason::Workers { unmet, .. }))) =
            &outcome
            && unmet.is_empty()
        {
            return Ok(Some(self.attempt_graph_unstick(evaluation_id, worker_caps).await?));
        }

        Ok(outcome)
    }

    /// Decide `Building` vs `Waiting` for an eval's current pending anchors.
    /// Returns `None` when nothing is pending (nothing to decide).
    async fn assess_buildability(
        &self,
        evaluation_id: EvaluationId,
        worker_caps: &[(Vec<String>, Vec<String>)],
    ) -> Result<Option<(EvaluationStatus, Option<WaitingReason>)>> {
        let pending = self.eval_pending_anchors(evaluation_id).await?;
        if pending.is_empty() {
            return Ok(None);
        }

        let arches: std::collections::HashSet<String> =
            worker_caps.iter().flat_map(|(a, _)| a.iter().cloned()).collect();
        let checker = BuildabilityChecker::load(self.state, &pending, arches, evaluation_id).await?;
        let target = if checker.any_buildable(&pending, worker_caps) {
            EvaluationStatus::Building
        } else {
            EvaluationStatus::Waiting
        };
        let reason = if matches!(target, EvaluationStatus::Waiting) {
            Some(checker.compute_waiting_reason(&pending, worker_caps))
        } else {
            None
        };

        Ok(Some((target, reason)))
    }

    /// Self-heal a graph-stuck evaluation: reconcile stale `closure_complete`
    /// flags to a fixpoint and re-promote, then re-assess. Recovers to `Building`
    /// when the heal frees a dispatchable anchor; otherwise reports `GraphStuck`
    /// with the blocked count so the stall is legible while later passes retry.
    async fn attempt_graph_unstick(
        &self,
        evaluation_id: EvaluationId,
        worker_caps: &[(Vec<String>, Vec<String>)],
    ) -> Result<(EvaluationStatus, Option<WaitingReason>)> {
        let db = &self.state.worker_db;
        info!(%evaluation_id, "graph stuck: pool can build every pending anchor but none is dispatchable; self-healing closure_complete");

        if let Err(e) = gradient_db::reconcile_closure_complete(db).await {
            error!(error = %e, %evaluation_id, "reconcile_closure_complete during graph-unstick failed");
        }

        match gradient_db::promote_ready(db).await {
            Ok(queued) => {
                gradient_db::notify_build_status_for_derivations(&self.state.db(), &queued).await
            }
            Err(e) => error!(error = %e, %evaluation_id, "promote_ready during graph-unstick failed"),
        }

        if let Some((EvaluationStatus::Building, reason)) =
            self.assess_buildability(evaluation_id, worker_caps).await?
        {
            return Ok((EvaluationStatus::Building, reason));
        }

        let blocked = self.eval_pending_anchors(evaluation_id).await?.len() as u32;

        Ok((EvaluationStatus::Waiting, Some(WaitingReason::graph_stuck(blocked))))
    }

    /// The non-terminal `derivation_build` anchors an evaluation still needs:
    /// the anchors of its `build_job`s in Created/Queued/Building/FailedTransient.
    async fn eval_pending_anchors(
        &self,
        evaluation_id: EvaluationId,
    ) -> Result<Vec<MDerivationBuild>> {
        use sea_orm::QuerySelect;
        let db = &self.state.worker_db;
        let anchor_ids: Vec<DerivationBuildId> = EBuildJob::find()
            .select_only()
            .column(CBuildJob::DerivationBuild)
            .filter(CBuildJob::Evaluation.eq(evaluation_id))
            .into_tuple::<DerivationBuildId>()
            .all(db)
            .await
            .context("fetch eval build_job anchors")?;
        if anchor_ids.is_empty() {
            return Ok(vec![]);
        }

        gradient_db::fetch_in_chunks(&anchor_ids, |chunk| async move {
            EDerivationBuild::find()
                .filter(CDerivationBuild::Id.is_in(chunk))
                .filter(CDerivationBuild::Status.is_in(vec![
                    BuildStatus::Created,
                    BuildStatus::Queued,
                    BuildStatus::Building,
                    BuildStatus::FailedTransient,
                ]))
                .all(db)
                .await
        })
        .await
        .context("fetch pending anchors")
    }
}

/// The capability a pre-build evaluation needs to make progress: `Fetching`
/// wants a fetch-capable worker, every other pre-build state wants an
/// eval-capable one.
fn pre_build_capability(status: EvaluationStatus) -> Option<EvalCapability> {
    match status {
        EvaluationStatus::Fetching => Some(EvalCapability::Fetch),
        EvaluationStatus::Queued
        | EvaluationStatus::EvaluatingFlake
        | EvaluationStatus::EvaluatingDerivation => Some(EvalCapability::Eval),
        _ => None,
    }
}

/// Whether the connected pool provides `capability`.
fn capability_available(
    capability: EvalCapability,
    eval_capable_workers: usize,
    fetch_capable_workers: usize,
) -> bool {
    match capability {
        EvalCapability::Fetch => fetch_capable_workers > 0,
        EvalCapability::Eval => eval_capable_workers > 0,
    }
}

/// Decide whether an *active* (non-`Waiting`) pre-build evaluation must stall.
///
/// Returns `Some((Waiting, reason))` when no worker provides the capability the
/// state needs - a `Fetching` eval needs `fetch`, every other pre-build state
/// needs `eval`. Returns `None` while the eval can progress. `Waiting` evals are
/// handled by [`decide_eval_recovery`], so this returns `None` for them.
fn decide_pre_build_target(
    current: EvaluationStatus,
    eval_capable_workers: usize,
    fetch_capable_workers: usize,
    connected_workers: u32,
) -> Option<(EvaluationStatus, Option<WaitingReason>)> {
    let capability = pre_build_capability(current)?;
    if capability_available(capability, eval_capable_workers, fetch_capable_workers) {
        return None;
    }

    Some((
        EvaluationStatus::Waiting,
        Some(WaitingReason::eval_workers(capability, connected_workers)),
    ))
}

/// Recovery for a `Waiting` eval parked in a pre-build phase: unpark to `Queued`
/// once `capability` is back, otherwise refresh the reason with the live count.
fn decide_eval_recovery(
    capability: EvalCapability,
    eval_capable_workers: usize,
    fetch_capable_workers: usize,
    connected_workers: u32,
) -> (EvaluationStatus, Option<WaitingReason>) {
    if capability_available(capability, eval_capable_workers, fetch_capable_workers) {
        (EvaluationStatus::Queued, None)
    } else {
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::eval_workers(capability, connected_workers)),
        )
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

/// Re-queue the in-flight jobs orphaned by a worker disconnect so they
/// re-dispatch instead of lingering in a non-terminal DB status. Anchors move
/// `Building -> Queued`; evaluations (which the state machine only lets reach
/// `Queued` via `Waiting`) park to `Waiting` so the reconciler that runs right
/// after recovers them to `Queued` once an eval-capable worker is free.
pub async fn requeue_orphaned_jobs(state: &Arc<ServerState>, orphaned: &[PendingJob]) {
    for job in orphaned {
        if let Some(derivation_build) = job.derivation_build() {
            match EDerivationBuild::find_by_id(derivation_build).one(&state.worker_db).await {
                Ok(Some(anchor)) if anchor.status == BuildStatus::Building => {
                    update_derivation_build_status(&state.db(), anchor, BuildStatus::Queued).await;
                }
                Ok(_) => {}
                Err(e) => warn!(error = %e, %derivation_build, "requeue orphaned build: load failed"),
            }

            continue;
        }

        let evaluation_id = job.evaluation_id();
        match EEvaluation::find_by_id(evaluation_id).one(&state.worker_db).await {
            Ok(Some(eval))
                if matches!(
                    eval.status,
                    EvaluationStatus::Fetching
                        | EvaluationStatus::EvaluatingFlake
                        | EvaluationStatus::EvaluatingDerivation
                ) =>
            {
                persist_waiting_reason(
                    state,
                    eval.id,
                    &eval.waiting_reason,
                    Some(&WaitingReason::eval_workers(EvalCapability::Eval, 0)),
                )
                .await;
                update_evaluation_status(&state.db(), eval, EvaluationStatus::Waiting).await;
            }
            Ok(_) => {}
            Err(e) => warn!(error = %e, %evaluation_id, "requeue orphaned eval: load failed"),
        }
    }
}

// ---------------------------------------------------------------------------
// Public free-function API (thin wrappers around BuildStateHandler)
// ---------------------------------------------------------------------------

pub async fn handle_build_output(
    state: &Arc<ServerState>,
    job: &PendingBuildJob,
    derivation_build: DerivationBuildId,
    outputs: Vec<BuildOutput>,
    metrics: Option<BuildMetrics>,
    substituted: bool,
) -> Result<()> {
    BuildStateHandler::new(state)
        .handle_build_output(job, derivation_build, outputs, metrics, substituted)
        .await
}

pub async fn handle_build_job_completed(
    state: &Arc<ServerState>,
    derivation_build: DerivationBuildId,
) -> Result<()> {
    BuildStateHandler::new(state)
        .handle_build_job_completed(derivation_build)
        .await
}

pub async fn handle_build_job_failed(
    state: &Arc<ServerState>,
    derivation_build: DerivationBuildId,
    error: &str,
    kind: BuildFailureKind,
    missing_paths: &[String],
) -> Result<()> {
    BuildStateHandler::new(state)
        .handle_build_job_failed(derivation_build, error, kind, missing_paths)
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
    fetch_capable_workers: usize,
    draining: bool,
) -> Result<()> {
    BuildStateHandler::new(state)
        .reconcile_waiting_state(worker_caps, eval_capable_workers, fetch_capable_workers, draining)
        .await
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Pre-loaded derivation and feature data for a set of pending anchors.
///
/// Used by [`BuildStateHandler::reconcile_waiting_state`] to determine whether
/// any pending anchor can be satisfied by the current worker pool without
/// re-querying the DB per evaluation.
struct BuildabilityChecker {
    drv_by_id: HashMap<DerivationId, MDerivation>,
    /// derivation_build → `SubstituteUnavailable` miss count. A substitutable
    /// anchor is only treated as buildable-anywhere while it is below the
    /// escalation threshold; past it, it is checked against real arch/features
    /// like any other anchor (so the parker can park it when no arch worker exists).
    substitute_misses: HashMap<DerivationBuildId, i64>,
    substitute_miss_escalation_threshold: i64,
    /// Maps derivation ID → list of required feature IDs.
    features_by_drv: HashMap<DerivationId, Vec<FeatureId>>,
    feature_name: HashMap<FeatureId, String>,
    connected_architectures: std::collections::HashSet<String>,
}

impl BuildabilityChecker {
    /// Query the DB for all derivations, required features, and substitute-miss
    /// counts referenced by `anchors`, returning a checker ready to call
    /// [`any_buildable`].
    ///
    /// [`any_buildable`]: BuildabilityChecker::any_buildable
    async fn load(
        state: &Arc<ServerState>,
        anchors: &[MDerivationBuild],
        connected_architectures: std::collections::HashSet<String>,
        evaluation_id: EvaluationId,
    ) -> Result<Self> {
        let db = &state.worker_db;
        let drv_ids: Vec<DerivationId> = anchors.iter().map(|a| a.derivation).collect();
        let anchor_ids: Vec<DerivationBuildId> = anchors.iter().map(|a| a.id).collect();
        // A count-query failure → 0 misses → substitute-mode, same as the dispatch side.
        // Scoped to this evaluation so a fresh eval is not parked on a previous
        // eval's exhausted substitute budget.
        let substitute_misses = gradient_db::substitute_miss_counts(db, &anchor_ids)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|((anchor, eval), misses)| (eval == evaluation_id).then_some((anchor, misses)))
            .collect();

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
            substitute_miss_escalation_threshold: state
                .config
                .eval
                .substitute_miss_escalation_threshold as i64,
            features_by_drv,
            feature_name,
            connected_architectures,
        })
    }

    /// Whether any pending anchor can run on the connected pool. A `Queued` or
    /// `Building` anchor already has all dependency anchors terminal-success
    /// (the promotion invariant), so it is dispatchable; a `Created` anchor is
    /// still blocked on deps. Substitutable anchors run anywhere until they
    /// exhaust the miss budget.
    fn any_buildable(&self, anchors: &[MDerivationBuild], worker_caps: &[(Vec<String>, Vec<String>)]) -> bool {
        anchors.iter().any(|a| {
            if a.status == BuildStatus::Building {
                return true;
            }
            if a.status != BuildStatus::Queued {
                return false;
            }
            let Some(drv) = self.drv_by_id.get(&a.derivation) else {
                return false;
            };
            let miss = self.substitute_misses.get(&a.id).copied().unwrap_or(0);
            let arch_has_worker = arch_available(&self.connected_architectures, &drv.architecture);
            match decide_dispatch_mode(
                a.substitutable,
                miss,
                self.substitute_miss_escalation_threshold,
                arch_has_worker,
            ) {
                BuildDispatchMode::SubstituteBuiltin => true,
                BuildDispatchMode::SubstituteStalled => false,
                BuildDispatchMode::RealArch => {
                    let required: Vec<&str> = self.required_features_for(&a.derivation);
                    worker_caps.iter().any(|(arch, feats)| {
                        let arch_ok = drv.architecture == "builtin"
                            || arch.iter().any(|a| a == &drv.architecture);
                        let feats_ok = required.iter().all(|f| feats.iter().any(|sf| sf == f));
                        arch_ok && feats_ok
                    })
                }
            }
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
    /// the number of pending anchors it covers. Used for the API
    /// `waiting_reason` payload so the UI can explain *why* nothing is
    /// dispatching.
    fn compute_waiting_reason(
        &self,
        anchors: &[MDerivationBuild],
        worker_caps: &[(Vec<String>, Vec<String>)],
    ) -> WaitingReason {
        let mut grouped: BTreeMap<(String, Vec<String>), u32> = BTreeMap::new();
        for a in anchors {
            let miss = self.substitute_misses.get(&a.id).copied().unwrap_or(0);
            let arch_has_worker = self
                .drv_by_id
                .get(&a.derivation)
                .map(|d| arch_available(&self.connected_architectures, &d.architecture))
                .unwrap_or(false);
            if matches!(
                decide_dispatch_mode(a.substitutable, miss, self.substitute_miss_escalation_threshold, arch_has_worker),
                BuildDispatchMode::SubstituteBuiltin
            ) {
                continue;
            }
            let Some(drv) = self.drv_by_id.get(&a.derivation) else {
                continue;
            };
            let required_owned: Vec<String> = self
                .required_features_for(&a.derivation)
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
    use super::{FailureOutcome, decide_failure_outcome, retry_backoff_elapsed, store_path_hash};
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
    #[test]
    fn substitute_miss_requeues_but_real_failures_cap_at_three() {
        assert!(matches!(decide_failure_outcome(BuildFailureKind::SubstituteUnavailable, 0, 3), FailureOutcome::Requeue));
        assert!(matches!(decide_failure_outcome(BuildFailureKind::SubstituteUnavailable, 99, 3), FailureOutcome::Requeue));
        assert!(matches!(decide_failure_outcome(BuildFailureKind::Transient, 0, 3), FailureOutcome::Retry));
        assert!(matches!(decide_failure_outcome(BuildFailureKind::Transient, 1, 3), FailureOutcome::Retry));
        assert!(matches!(decide_failure_outcome(BuildFailureKind::Transient, 2, 3), FailureOutcome::Permanent));
    }
    #[test]
    fn inputs_unavailable_is_terminal_no_retry() {
        for attempt in [0, 1, 99] {
            assert_eq!(
                decide_failure_outcome(BuildFailureKind::InputsUnavailable, attempt, 3),
                FailureOutcome::Permanent
            );
        }
    }
    #[test]
    fn store_path_hash_extracts_32_char_hash() {
        assert_eq!(
            store_path_hash("/nix/store/g9y0fvqh2c991vjprgz9mvdm0zj7ggij-python3-static-3.13"),
            Some("g9y0fvqh2c991vjprgz9mvdm0zj7ggij")
        );
        assert_eq!(store_path_hash("not-a-store-path"), None);
        assert_eq!(store_path_hash("/nix/store/"), None);
    }

    #[test]
    fn terminal_status_is_substituted_only_when_outputs_were_already_valid() {
        use super::terminal_success_status;
        use gradient_entity::build::BuildStatus;
        assert_eq!(terminal_success_status(true), BuildStatus::Substituted);
        assert_eq!(terminal_success_status(false), BuildStatus::Completed);
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

    fn eval_workers_view(r: &WaitingReason) -> (EvalCapability, u32) {
        match r {
            WaitingReason::EvalWorkers {
                capability,
                connected_workers,
            } => (*capability, *connected_workers),
            other => panic!("expected EvalWorkers variant, got {other:?}"),
        }
    }

    fn drv(id: DerivationId, arch: &str) -> MDerivation {
        gradient_entity::derivation::Model {
            id,
            hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            name: "x".into(),
            architecture: arch.into(),
            created_at: chrono::NaiveDateTime::default(),
            ..Default::default()
        }
    }

    fn build_for(drv_id: DerivationId, _eval_id: EvaluationId) -> MDerivationBuild {
        gradient_entity::derivation_build::Model {
            id: DerivationBuildId::now_v7(),
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
            substitute_miss_escalation_threshold: 2,
            features_by_drv,
            feature_name,
            connected_architectures: std::collections::HashSet::new(),
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

    /// Regression for issue #268/#381: a Queued evaluation whose connected
    /// workers lack the `eval` capability stalls to Waiting with an `eval`
    /// `EvalWorkers` reason, reporting the total connected pool size.
    #[test]
    fn pre_build_target_queued_no_eval_worker_stalls_to_eval_waiting() {
        let (target, reason) = decide_pre_build_target(EvaluationStatus::Queued, 0, 1, 3)
            .expect("stall must produce a transition");
        assert_eq!(target, EvaluationStatus::Waiting);
        let (cap, connected) = eval_workers_view(&reason.expect("stall carries a reason"));
        assert_eq!(cap, EvalCapability::Eval);
        assert_eq!(connected, 3);
    }

    /// #381: a `Fetching` eval needs a fetch-capable worker - an eval-only pool
    /// still strands it, with a `fetch` reason.
    #[test]
    fn pre_build_target_fetching_no_fetch_worker_stalls_to_fetch_waiting() {
        let (target, reason) = decide_pre_build_target(EvaluationStatus::Fetching, 2, 0, 2)
            .expect("stall must produce a transition");
        assert_eq!(target, EvaluationStatus::Waiting);
        let (cap, connected) = eval_workers_view(&reason.expect("stall carries a reason"));
        assert_eq!(cap, EvalCapability::Fetch);
        assert_eq!(connected, 2);
    }

    #[test]
    fn pre_build_target_active_pre_build_with_capability_left_alone() {
        // Fetching needs fetch; Queued/Evaluating* need eval. With both present
        // the eval is progressing and must not be reconciled.
        for status in [
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
            EvaluationStatus::Queued,
        ] {
            assert!(
                decide_pre_build_target(status, 1, 1, 1).is_none(),
                "{status:?} with capable workers must not be reconciled"
            );
        }
    }

    #[test]
    fn pre_build_target_ignores_waiting() {
        // Waiting recovery is decide_eval_recovery's job, not this function's.
        assert!(decide_pre_build_target(EvaluationStatus::Waiting, 0, 0, 0).is_none());
        assert!(decide_pre_build_target(EvaluationStatus::Waiting, 2, 2, 2).is_none());
    }

    #[test]
    fn eval_recovery_unparks_to_queued_when_capability_returns() {
        let (target, reason) = decide_eval_recovery(EvalCapability::Eval, 1, 0, 1);
        assert_eq!(target, EvaluationStatus::Queued);
        assert!(reason.is_none());

        let (target, reason) = decide_eval_recovery(EvalCapability::Fetch, 0, 1, 1);
        assert_eq!(target, EvaluationStatus::Queued);
        assert!(reason.is_none());
    }

    #[test]
    fn eval_recovery_refreshes_reason_while_capability_absent() {
        let (target, reason) = decide_eval_recovery(EvalCapability::Fetch, 5, 0, 5);
        assert_eq!(target, EvaluationStatus::Waiting);
        let (cap, connected) = eval_workers_view(&reason.expect("refresh carries a reason"));
        assert_eq!(cap, EvalCapability::Fetch);
        assert_eq!(connected, 5);
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

    fn substitutable_build(drv_id: DerivationId, _eval_id: EvaluationId) -> MDerivationBuild {
        gradient_entity::derivation_build::Model {
            id: DerivationBuildId::now_v7(),
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
        checker.substitute_misses.insert(build.id, 1);

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
        checker.substitute_misses.insert(build.id, 2);

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

    #[test]
    fn stalled_substitute_is_not_buildable_and_appears_in_unmet() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "i686-linux");
        let mut b = build_for(d.id, eval_id);
        b.substitutable = true;
        let mut checker = checker_with(vec![d.clone()], vec![]);
        checker.substitute_misses.insert(b.id, 2);
        checker.connected_architectures.insert("x86_64-linux".into());
        let caps = vec![(vec!["x86_64-linux".to_string()], vec![])];
        assert!(!checker.any_buildable(&[b.clone()], &caps));
        let reason = checker.compute_waiting_reason(&[b], &caps);
        let (unmet, _, available) = workers_view(&reason);
        assert!(unmet.iter().any(|u| u.architecture == "i686-linux"));
        assert_eq!(available, ["x86_64-linux"]);
    }

    #[test]
    fn dependency_blocked_anchor_is_not_buildable() {
        // A `Created` anchor still has unsatisfied dependency anchors, so it is
        // not dispatchable even when a matching worker is connected.
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "x86_64-linux");
        let mut b = build_for(d.id, eval_id);
        b.status = BuildStatus::Created;
        let mut checker = checker_with(vec![d], vec![]);
        checker.connected_architectures.insert("x86_64-linux".into());
        let caps = vec![(vec!["x86_64-linux".to_string()], vec![])];
        assert!(!checker.any_buildable(&[b], &caps));
    }

    #[test]
    fn substitutable_within_budget_is_buildable_anywhere() {
        let eval_id = EvaluationId::now_v7();
        let d = drv(DerivationId::now_v7(), "i686-linux");
        let mut b = build_for(d.id, eval_id);
        b.substitutable = true;
        let checker = checker_with(vec![d], vec![]);
        let caps = vec![(vec!["x86_64-linux".to_string()], vec![])];
        assert!(checker.any_buildable(&[b], &caps));
    }
}
