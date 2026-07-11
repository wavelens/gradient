/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Build output/completion/failure handling and retry policy.

use std::sync::Arc;

use anyhow::{Context, Result};

use gradient_core::ServerState;
use gradient_db::{
    cascade_dependency_failed, fail_latest_attempt, update_derivation_build_status,
    update_evaluation_status,
};
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
};
use tracing::{error, info, warn};

use super::self_heal::reconcile_missing_inputs;
use crate::jobs::{PendingBuildJob, PendingJob};
use crate::waiting_state::persist_waiting_reason;
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
        // A missing input self-heals: its producer is re-queued and rebuilds, so
        // the build retries in-eval like a transient failure and succeeds once
        // the input is back. The caller forces `Permanent` when the self-heal
        // circuit trips (the input is unrecoverable).
        BuildFailureKind::InputsUnavailable | BuildFailureKind::Transient => {
            if (attempt + 1) < max_attempts as i32 {
                FailureOutcome::Retry
            } else {
                FailureOutcome::Permanent
            }
        }
        // Eval-only kind (handled in `handle_eval_job_failed`); a build never
        // produces it, so treat it as terminal if one somehow reaches here.
        BuildFailureKind::CorruptEvalCache => FailureOutcome::Permanent,
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
        BuildFailureKind::SubstituteUnavailable => {
            Some(AttemptFailureReason::SubstituteUnavailable)
        }
        BuildFailureKind::InputsUnavailable => Some(AttemptFailureReason::InputsUnavailable),
        BuildFailureKind::Permanent => Some(AttemptFailureReason::BuilderNonzero),
        BuildFailureKind::Timeout => Some(AttemptFailureReason::WallClockTimeout),
        BuildFailureKind::Transient | BuildFailureKind::CorruptEvalCache => None,
    }
}

/// Circuit breaker for the `InputsUnavailable` self-heal. Each failed eval
/// reconciles the cache (purges the stale input) so the next eval rebuilds it; a
/// genuinely unrecoverable input turns that into a hot loop that churns the cache
/// forever. `prior_failures` is how many `InputsUnavailable` attempts this anchor
/// already has, so the self-heal runs for the first `max_loops` and the circuit
/// opens after - the build then fails fast without reconciling.
fn inputs_unavailable_circuit_open(prior_failures: i64, max_loops: u32) -> bool {
    prior_failures >= max_loops as i64
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

/// Cap a worker failure string before persisting it on `build_attempt`. The full
/// text already lands in the build log; the stored message is for quick surfacing,
/// so bound it on a char boundary to keep the row lean.
fn truncate_failure_message(error: &str) -> String {
    const MAX: usize = 8 * 1024;
    if error.len() <= MAX {
        return error.to_string();
    }

    let end = (0..=MAX)
        .rev()
        .find(|&i| error.is_char_boundary(i))
        .unwrap_or(0);
    format!("{} [truncated]", &error[..end])
}

pub async fn handle_build_output(
    state: &Arc<ServerState>,
    _job: &PendingBuildJob,
    derivation_build: DerivationBuildId,
    outputs: Vec<BuildOutput>,
    metrics: Option<BuildMetrics>,
    substituted: bool,
) -> Result<()> {
    let anchor = EDerivationBuild::find_by_id(derivation_build)
        .one(&state.worker_db)
        .await
        .context("fetch derivation_build")?
        .with_context(|| format!("derivation_build {} not found", derivation_build))?;

    let build_id = anchor.id;
    let derivation_id = anchor.derivation;

    // Per-build metrics: a multi-build job yields one `BuildOutput` per
    // build, so this records exactly one `derivation_metric` row per anchor.
    if let Some(metrics) = metrics {
        record_metrics(state, &anchor, derivation_id, &metrics).await;
    }

    for output in &outputs {
        let existing = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.eq(derivation_id))
            .filter(CDerivationOutput::Name.eq(&output.name))
            .one(&state.worker_db)
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
            if let Err(e) = active.update(&state.worker_db).await {
                error!(error = %e, %build_id, output_name = %output.name, "failed to update derivation_output");
            }

            // Delete any prior products for this output (idempotency on retry).
            if let Err(e) = EBuildProduct::delete_many()
                .filter(CBuildProduct::DerivationOutput.eq(row_id))
                .exec(&state.worker_db)
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

                if let Err(e) = am.insert(&state.worker_db).await {
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
        if let Err(e) = active.update(&state.worker_db).await {
            warn!(%build_id, error = %e, "failed to record anchor as substituted");
        }
    }

    Ok(())
}

pub async fn handle_build_job_completed(
    state: &Arc<ServerState>,
    derivation_build: DerivationBuildId,
) -> Result<()> {
    let anchor = match EDerivationBuild::find_by_id(derivation_build)
        .one(&state.worker_db)
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
    update_derivation_build_status(&state.db(), anchor, terminal).await;

    if was_external_cached {
        let state = Arc::clone(state);
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

    check_referencing_evals_done(state, derivation_id).await
}

/// Insert a `derivation_metric` history row from a build's worker metrics.
/// Called once per build from the `BuildOutput` handler.
async fn record_metrics(
    state: &Arc<ServerState>,
    anchor: &MDerivationBuild,
    derivation_id: DerivationId,
    metrics: &BuildMetrics,
) {
    let (pname, closure_size) = match EDerivation::find_by_id(derivation_id)
        .one(&state.worker_db)
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
        worker_id: gradient_db::latest_attempt_worker(&state.worker_db, anchor.id)
            .await
            .ok()
            .flatten()
            .unwrap_or_default(),
        created_at: gradient_types::now(),
    }
    .into_active_model();

    if let Err(e) = metric.insert(&state.worker_db).await {
        warn!(%derivation_id, error = %e, "failed to record derivation_metric");
    }
}

pub async fn handle_build_job_failed(
    state: &Arc<ServerState>,
    derivation_build: DerivationBuildId,
    error: &str,
    kind: BuildFailureKind,
    missing_paths: &[String],
) -> Result<()> {
    let anchor = match EDerivationBuild::find_by_id(derivation_build)
        .one(&state.worker_db)
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
    if let Some(attempt_id) = gradient_db::latest_attempt_id(&state.worker_db, anchor.id)
        .await
        .ok()
        .flatten()
        && let Err(e) = state
            .log_storage
            .append(attempt_id, &format!("\n=== build failed: {error} ===\n"))
            .await
    {
        warn!(%derivation_build, error = %e, "failed to append worker error to build log");
    }

    let derivation_id = anchor.derivation;
    let attempt = anchor.attempt;
    let max_attempts = state.config.eval.build_max_attempts;

    // Count past `InputsUnavailable` self-heal loops before recording this
    // failure, so the breaker decision excludes the attempt we're about to mark.
    let prior_inputs_unavailable = if matches!(kind, BuildFailureKind::InputsUnavailable) {
        gradient_db::inputs_unavailable_attempt_count(&state.worker_db, derivation_build)
            .await
            .unwrap_or(0)
    } else {
        0
    };

    if let Err(e) = fail_latest_attempt(
        &state.worker_db,
        derivation_build,
        AttemptOutcome::Failed,
        attempt_reason(kind),
        Some(truncate_failure_message(error)),
    )
    .await
    {
        warn!(%derivation_build, error = %e, "failed to record attempt failure reason");
    }

    // Self-heal: a required input was reported absent from the cache while
    // its producer was marked done/substituted. Purge those stale outputs so
    // the producer rebuilds and this build retries in-eval. The circuit
    // breaker caps the self-heal at `inputs_unavailable_max_loops`: an
    // unrecoverable input would otherwise churn the cache forever.
    let max_loops = state.config.eval.inputs_unavailable_max_loops;
    let inputs_circuit_open = matches!(kind, BuildFailureKind::InputsUnavailable)
        && inputs_unavailable_circuit_open(prior_inputs_unavailable, max_loops);
    if matches!(kind, BuildFailureKind::InputsUnavailable) && !missing_paths.is_empty() {
        if inputs_circuit_open {
            warn!(
                %derivation_build,
                prior_failures = prior_inputs_unavailable,
                max_loops,
                "InputsUnavailable self-heal circuit open; failing without reconcile to break the hot loop"
            );
        } else if let Err(e) = reconcile_missing_inputs(state, derivation_id, missing_paths).await {
            warn!(%derivation_build, error = %e, "failed to reconcile missing inputs");
        }
    }

    // `InputsUnavailable` retries in-eval (the self-heal re-queues its input),
    // but once the breaker trips the input is unrecoverable - stop retrying.
    let outcome = match decide_failure_outcome(kind, attempt, max_attempts) {
        FailureOutcome::Retry if inputs_circuit_open => FailureOutcome::Permanent,
        other => other,
    };
    match outcome {
        FailureOutcome::Retry => {
            let mut active: ADerivationBuild = anchor.clone().into_active_model();
            active.attempt = Set(attempt + 1);
            if let Err(e) = active.update(&state.worker_db).await {
                error!(%derivation_build, error = %e, "failed to bump anchor attempt");
            }
            let reloaded = EDerivationBuild::find_by_id(derivation_build)
                .one(&state.worker_db)
                .await?
                .unwrap_or(anchor);
            update_derivation_build_status(&state.db(), reloaded, BuildStatus::FailedTransient)
                .await;
            info!(%derivation_build, attempt = attempt + 1, "transient build failure; scheduled for retry");
            return Ok(());
        }
        FailureOutcome::Requeue => {
            // Substitute miss: back to the queue without an `attempt` bump
            // or a permanent mark. Dispatch escalates to a real build once
            // the substitute-miss count crosses the threshold. Dependents are
            // untouched - nothing failed.
            update_derivation_build_status(&state.db(), anchor, BuildStatus::Queued).await;
            info!(%derivation_build, "substitute unavailable; re-queued for re-dispatch/escalation");
            return Ok(());
        }
        FailureOutcome::Permanent => {
            update_derivation_build_status(&state.db(), anchor, BuildStatus::FailedPermanent).await;
        }
        FailureOutcome::Timeout => {
            update_derivation_build_status(&state.db(), anchor, BuildStatus::FailedTimeout).await;
        }
    }

    // `update_derivation_build_status` already cascades dependency failure on
    // a terminal-failure transition; this is a belt-and-braces global cascade
    // over the same graph, idempotent on already-failed anchors.
    cascade_dependency_failed(&state.worker_db, derivation_id).await?;
    check_referencing_evals_done(state, derivation_id).await
}

/// After an anchor reaches a terminal status, sweep every evaluation that
/// references the derivation and finalize the settled ones. Idempotent
/// belt-and-braces around the emitter's own finalize (which is skipped when
/// the state machine rejects a racing transition).
async fn check_referencing_evals_done(
    state: &Arc<ServerState>,
    derivation: DerivationId,
) -> Result<()> {
    gradient_db::finalize_evals_for_derivations(&state.db(), &[derivation]).await?;
    Ok(())
}

/// Re-queue the in-flight jobs orphaned by a worker disconnect so they
/// re-dispatch instead of lingering in a non-terminal DB status. Anchors move
/// `Building -> Queued`; evaluations (which the state machine only lets reach
/// `Queued` via `Waiting`) park to `Waiting` so the reconciler that runs right
/// after recovers them to `Queued` once an eval-capable worker is free.
pub async fn requeue_orphaned_jobs(state: &Arc<ServerState>, orphaned: &[PendingJob]) {
    for job in orphaned {
        if let Some(derivation_build) = job.derivation_build() {
            match EDerivationBuild::find_by_id(derivation_build)
                .one(&state.worker_db)
                .await
            {
                Ok(Some(anchor)) if anchor.status == BuildStatus::Building => {
                    update_derivation_build_status(&state.db(), anchor, BuildStatus::Queued).await;
                }
                Ok(_) => {}
                Err(e) => {
                    warn!(error = %e, %derivation_build, "requeue orphaned build: load failed")
                }
            }

            continue;
        }

        let evaluation_id = job.evaluation_id();
        match EEvaluation::find_by_id(evaluation_id)
            .one(&state.worker_db)
            .await
        {
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

#[cfg(test)]
mod retry_tests {
    use super::{
        FailureOutcome, decide_failure_outcome, inputs_unavailable_circuit_open,
        retry_backoff_elapsed, truncate_failure_message,
    };
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
        assert!(!retry_backoff_elapsed(
            1,
            t0,
            t0 + chrono::Duration::seconds(29),
            30
        ));
        assert!(retry_backoff_elapsed(
            1,
            t0,
            t0 + chrono::Duration::seconds(30),
            30
        ));
        assert!(!retry_backoff_elapsed(
            2,
            t0,
            t0 + chrono::Duration::seconds(59),
            30
        ));
        assert!(retry_backoff_elapsed(
            2,
            t0,
            t0 + chrono::Duration::seconds(60),
            30
        ));
    }
    #[test]
    fn substitute_miss_requeues_but_real_failures_cap_at_three() {
        assert!(matches!(
            decide_failure_outcome(BuildFailureKind::SubstituteUnavailable, 0, 3),
            FailureOutcome::Requeue
        ));
        assert!(matches!(
            decide_failure_outcome(BuildFailureKind::SubstituteUnavailable, 99, 3),
            FailureOutcome::Requeue
        ));
        assert!(matches!(
            decide_failure_outcome(BuildFailureKind::Transient, 0, 3),
            FailureOutcome::Retry
        ));
        assert!(matches!(
            decide_failure_outcome(BuildFailureKind::Transient, 1, 3),
            FailureOutcome::Retry
        ));
        assert!(matches!(
            decide_failure_outcome(BuildFailureKind::Transient, 2, 3),
            FailureOutcome::Permanent
        ));
    }
    #[test]
    fn inputs_unavailable_retries_like_transient_then_permanent() {
        // A missing input is self-healed (its producer is re-queued) and the
        // build retries in-eval, so it behaves like a transient failure up to the
        // attempt budget rather than failing permanently on the first miss.
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::InputsUnavailable, 0, 3),
            FailureOutcome::Retry
        );
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::InputsUnavailable, 1, 3),
            FailureOutcome::Retry
        );
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::InputsUnavailable, 2, 3),
            FailureOutcome::Permanent
        );
    }
    #[test]
    fn inputs_unavailable_circuit_opens_after_max_loops() {
        // First `max_loops` failures self-heal; the next opens the circuit.
        assert!(!inputs_unavailable_circuit_open(0, 3));
        assert!(!inputs_unavailable_circuit_open(1, 3));
        assert!(!inputs_unavailable_circuit_open(2, 3));
        assert!(inputs_unavailable_circuit_open(3, 3));
        assert!(inputs_unavailable_circuit_open(7, 3));
    }
    #[test]
    fn truncate_failure_message_bounds_long_input_on_char_boundary() {
        assert_eq!(truncate_failure_message("short error"), "short error");
        let long = "é".repeat(8 * 1024);
        let out = truncate_failure_message(&long);
        assert!(out.len() <= 8 * 1024 + " [truncated]".len());
        assert!(out.ends_with(" [truncated]"));
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn terminal_status_is_substituted_only_when_outputs_were_already_valid() {
        use super::terminal_success_status;
        use gradient_entity::build::BuildStatus;
        assert_eq!(terminal_success_status(true), BuildStatus::Substituted);
        assert_eq!(terminal_success_status(false), BuildStatus::Completed);
    }
}
