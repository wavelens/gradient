/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! One handler per [`Transition`], each run inside the actor's transaction.

use std::collections::HashMap;

use anyhow::{Context, Result};
use gradient_db::{
    DbContext, cascade_dependency_failed, fail_latest_attempt, succeed_latest_attempt,
    update_derivation_build_status, update_evaluation_status, update_evaluation_status_with_error,
};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::proto::{BuildFailureKind, BuildMetrics, BuildOutput};
use gradient_types::*;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
};
use tracing::{error, info, warn};

use crate::ingest::{EvalEdgeAccumulator, flush_deferred_deps};
use crate::messages::{SubstituteLog, Transition, TransitionReport};
use crate::policy::{self, FailureOutcome};

pub(crate) async fn apply(
    ctx: &DbContext,
    edges: &mut HashMap<EvaluationId, EvalEdgeAccumulator>,
    transition: Transition,
) -> Result<TransitionReport> {
    match transition {
        Transition::EvalStreamCompleted { evaluation } => {
            let pending = edges.remove(&evaluation).unwrap_or_default().into_pending();
            if let Err(e) = flush_deferred_deps(&ctx.worker_db, evaluation, pending).await {
                error!(error = %e, evaluation_id = %evaluation, "flush_deferred_deps failed");
            }

            eval_stream_completed(ctx, evaluation).await?;
            Ok(TransitionReport::default())
        }
        Transition::EvalFailed {
            evaluation,
            error,
            kind,
            missing_paths,
        } => {
            edges.remove(&evaluation);
            eval_failed(ctx, evaluation, &error, kind, &missing_paths).await?;
            Ok(TransitionReport::default())
        }
        Transition::AbortEvaluation { evaluation } => {
            edges.remove(&evaluation);
            let Some(eval) = EEvaluation::find_by_id(evaluation)
                .one(&ctx.worker_db)
                .await?
            else {
                return Ok(TransitionReport::default());
            };

            let aborted_anchors = gradient_db::abort_evaluation(ctx, eval).await;
            Ok(TransitionReport {
                aborted_anchors,
                ..Default::default()
            })
        }
        Transition::BuildStarted { anchor } => {
            let Some(row) = EDerivationBuild::find_by_id(anchor)
                .one(&ctx.worker_db)
                .await?
            else {
                warn!(derivation_build = %anchor, "anchor not found for Building status update");
                return Ok(TransitionReport::default());
            };

            if row.status == BuildStatus::Aborted {
                return Ok(TransitionReport {
                    already_aborted: true,
                    ..Default::default()
                });
            }

            update_derivation_build_status(ctx, row, BuildStatus::Building).await;
            Ok(TransitionReport::default())
        }
        Transition::BuildOutput {
            anchor,
            outputs,
            metrics,
            substituted,
        } => {
            build_output(ctx, anchor, outputs, metrics, substituted).await?;
            Ok(TransitionReport::default())
        }
        Transition::BuildCompleted { anchor } => Ok(TransitionReport {
            substitute_log: build_completed(ctx, anchor).await?,
            ..Default::default()
        }),
        Transition::BuildFailed {
            anchor,
            error,
            log_banner,
            kind,
            missing_paths,
        } => {
            build_failed(ctx, anchor, &error, &log_banner, kind, &missing_paths).await?;
            Ok(TransitionReport::default())
        }
        Transition::Dispatched {
            evaluation,
            anchor,
            dispatched_job,
            substitute,
            build_context,
        } => {
            dispatched(
                ctx,
                evaluation,
                anchor,
                dispatched_job,
                substitute,
                build_context,
            )
            .await;
            Ok(TransitionReport::default())
        }
        Transition::OrphanedBuilds { anchors } => {
            for anchor in anchors {
                match EDerivationBuild::find_by_id(anchor)
                    .one(&ctx.worker_db)
                    .await
                {
                    Ok(Some(row)) if row.status == BuildStatus::Building => {
                        update_derivation_build_status(ctx, row, BuildStatus::Queued).await;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!(error = %e, derivation_build = %anchor, "requeue orphaned build: load failed")
                    }
                }
            }

            Ok(TransitionReport::default())
        }
        Transition::Ready {
            anchors,
            closure_sizes,
        } => {
            ready(ctx, &anchors, &closure_sizes).await?;
            Ok(TransitionReport::default())
        }
        Transition::Reconcile { scope } => {
            gradient_db::reconcile_build_graph(ctx, scope).await;
            Ok(TransitionReport::default())
        }
        Transition::AbortEvaluationAnchors { evaluation } => {
            edges.remove(&evaluation);
            let Some(eval) = EEvaluation::find_by_id(evaluation)
                .one(&ctx.worker_db)
                .await?
            else {
                return Ok(TransitionReport::default());
            };

            let aborted_anchors = gradient_db::abort_eval_anchors(ctx, &eval).await?;
            Ok(TransitionReport {
                aborted_anchors,
                ..Default::default()
            })
        }
    }
}

async fn eval_stream_completed(ctx: &DbContext, evaluation_id: EvaluationId) -> Result<()> {
    // The build graph is now complete: materialise each entry point's closure
    // and seed the per-entry-point dependency counts (#383).
    if let Err(e) = gradient_db::seed_entry_point_dep_counts(&ctx.worker_db, evaluation_id).await {
        error!(error = %e, %evaluation_id, "seed_entry_point_dep_counts failed (non-fatal)");
    }

    // The dependency graph is now complete (edges flushed): run the canonical
    // healing pipeline scoped to this eval, which marks its anchors
    // edges_complete, heals cache trust across its closure, reconciles the gate
    // flags, and promotes the ready frontier (see `gradient_db::reconcile`).
    gradient_db::reconcile_build_graph(ctx, gradient_db::ReconcileScope::Eval(evaluation_id)).await;

    // Promotion is graph-driven (gradient_db::promotion), independent of eval
    // completion, so finishing the stream just advances the eval to Building.
    if let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&ctx.worker_db)
        .await?
        && matches!(
            eval.status,
            EvaluationStatus::EvaluatingFlake | EvaluationStatus::EvaluatingDerivation
        )
    {
        info!(%evaluation_id, "eval job complete; promoting evaluation to Building");
        update_evaluation_status(ctx, eval, EvaluationStatus::Building).await;
    }

    // If every build was already terminal (e.g. all Substituted), close the
    // evaluation out via the shared decision function.
    gradient_db::check_evaluation_done(ctx, evaluation_id).await?;
    Ok(())
}

async fn eval_failed(
    ctx: &DbContext,
    evaluation_id: EvaluationId,
    error: &str,
    kind: BuildFailureKind,
    missing_paths: &[String],
) -> Result<()> {
    // Corrupt shared eval-cache: the worker already dropped its local copy, so
    // purge the poisoned shared blob and re-queue the eval to re-evaluate
    // cache-less. If it heals (blob existed), skip the terminal-Failed path.
    if kind == BuildFailureKind::CorruptEvalCache
        && let Some(fingerprint) = missing_paths.first()
        && heal_corrupt_eval_cache(ctx, evaluation_id, fingerprint).await?
    {
        return Ok(());
    }

    if let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&ctx.worker_db)
        .await?
        && !matches!(
            eval.status,
            EvaluationStatus::Completed | EvaluationStatus::Failed | EvaluationStatus::Aborted
        )
    {
        // The API writes `Aborted` before `AbortJob` goes out, so the guard above
        // normally catches this. If that write was lost, settle the evaluation
        // where the abort meant to put it rather than reporting a failure the
        // user did not cause.
        if kind == BuildFailureKind::Aborted {
            update_evaluation_status(ctx, eval, EvaluationStatus::Aborted).await;
            return Ok(());
        }

        update_evaluation_status_with_error(
            ctx,
            eval,
            EvaluationStatus::Failed,
            error.to_owned(),
            Some("worker".to_string()),
        )
        .await;
    }

    Ok(())
}

/// Purge a corrupt shared eval-cache blob and re-queue the evaluation. Returns
/// `true` when it re-queued. The blob's own existence is the circuit breaker:
/// the first corrupt failure finds the row and purges+re-queues it; once purged,
/// a recurring corruption (the freshly-generated cache is itself unreadable, i.e.
/// a broken worker/disk) has no shared blob to blame, so this returns `false` and
/// the caller fails the eval for real instead of looping.
async fn heal_corrupt_eval_cache(
    ctx: &DbContext,
    evaluation_id: EvaluationId,
    fingerprint: &str,
) -> Result<bool> {
    let purged = EEvalCacheStore::delete_many()
        .filter(CEvalCacheStore::Fingerprint.eq(fingerprint))
        .exec(&ctx.worker_db)
        .await?
        .rows_affected;
    if purged == 0 {
        warn!(%evaluation_id, %fingerprint, "corrupt eval-cache recurred with no shared blob to purge; failing eval");
        return Ok(false);
    }

    if let Err(e) = ctx.storage.nar_storage.delete_eval_cache(fingerprint).await {
        warn!(%fingerprint, error = %e, "failed to delete corrupt eval-cache object");
    }

    if let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&ctx.worker_db)
        .await?
        && !matches!(
            eval.status,
            EvaluationStatus::Completed | EvaluationStatus::Failed | EvaluationStatus::Aborted
        )
    {
        update_evaluation_status(ctx, eval, EvaluationStatus::Queued).await;
    }

    info!(%evaluation_id, %fingerprint, "purged corrupt eval-cache blob; re-queued eval for fresh evaluation");
    Ok(true)
}

async fn build_output(
    ctx: &DbContext,
    derivation_build: DerivationBuildId,
    outputs: Vec<BuildOutput>,
    metrics: Option<BuildMetrics>,
    substituted: bool,
) -> Result<()> {
    let anchor = EDerivationBuild::find_by_id(derivation_build)
        .one(&ctx.worker_db)
        .await
        .context("fetch derivation_build")?
        .with_context(|| format!("derivation_build {derivation_build} not found"))?;

    let build_id = anchor.id;
    let derivation_id = anchor.derivation;
    if let Some(metrics) = metrics {
        record_metrics(ctx, &anchor, derivation_id, &metrics).await;
    }

    for output in &outputs {
        let existing = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.eq(derivation_id))
            .filter(CDerivationOutput::Name.eq(&output.name))
            .one(&ctx.worker_db)
            .await
            .context("fetch derivation_output")?;

        let Some(row) = existing else {
            warn!(%build_id, output_name = %output.name, "derivation_output row not found");
            continue;
        };

        let row_id = row.id;
        let mut active = row.into_active_model();
        if let BuildOutputMetadata::Available {
            nar_size,
            nar_hash: _,
        } = output.nar_metadata()
        {
            active.nar_size = Set(Some(nar_size));
        }

        if let Err(e) = active.update(&ctx.worker_db).await {
            error!(error = %e, %build_id, output_name = %output.name, "failed to update derivation_output");
        }

        // Prior products are dropped first so a retry stays idempotent.
        if let Err(e) = EBuildProduct::delete_many()
            .filter(CBuildProduct::DerivationOutput.eq(row_id))
            .exec(&ctx.worker_db)
            .await
            .context("delete prior build_product rows")
        {
            warn!(error = %e, %build_id, output_name = %output.name, "failed to delete prior build_product rows");
        }

        for product in &output.products {
            let am = MBuildProduct {
                id: BuildProductId::now_v7(),
                derivation_output: row_id,
                file_type: product.file_type.clone(),
                subtype: product.subtype.clone(),
                name: product.name.clone(),
                path: product.path.clone(),
                size: product.size.map(|s| s as i64),
                created_at: now(),
            }
            .into_active_model();

            if let Err(e) = am.insert(&ctx.worker_db).await {
                warn!(error = %e, %build_id, output_name = %output.name, "failed to insert build_product");
            }
        }
    }

    info!(%build_id, output_count = outputs.len(), "build outputs recorded");

    // The daemon found the outputs already valid, but the worker has not pushed
    // their NARs yet: record the flag and let `build_completed` turn it into the
    // terminal status, after the push (#399, #303).
    if substituted {
        let mut active = anchor.into_active_model();
        active.substituted = Set(true);
        active.updated_at = Set(now());
        if let Err(e) = active.update(&ctx.worker_db).await {
            warn!(%build_id, error = %e, "failed to record anchor as substituted");
        }
    }

    Ok(())
}

async fn build_completed(
    ctx: &DbContext,
    derivation_build: DerivationBuildId,
) -> Result<Option<SubstituteLog>> {
    let Some(anchor) = EDerivationBuild::find_by_id(derivation_build)
        .one(&ctx.worker_db)
        .await?
    else {
        warn!(%derivation_build, "anchor not found on job_completed");
        return Ok(None);
    };

    let derivation_id = anchor.derivation;
    let was_external_cached = anchor.substitutable;

    // The output NARs are pushed by the time `JobCompleted` arrives, so the
    // anchor may now become dispatch-ready.
    let terminal = policy::terminal_success_status(anchor.substituted);
    if let Err(e) = succeed_latest_attempt(
        &ctx.worker_db,
        derivation_build,
        policy::terminal_success_outcome(anchor.substituted),
    )
    .await
    {
        warn!(%derivation_build, error = %e, "failed to record attempt success");
    }
    update_derivation_build_status(ctx, anchor, terminal).await;
    check_referencing_evals_done(ctx, derivation_id).await?;

    if !was_external_cached {
        return Ok(None);
    }

    match EDerivation::find_by_id(derivation_id)
        .one(&ctx.worker_db)
        .await
    {
        Ok(Some(d)) => Ok(Some(SubstituteLog {
            anchor: derivation_build,
            derivation: derivation_id,
            drv_path: d.drv_path(),
        })),
        Ok(None) => {
            warn!(%derivation_build, %derivation_id, "substitute_log: derivation row missing");
            Ok(None)
        }
        Err(e) => {
            warn!(%derivation_build, error = %e, "substitute_log: derivation lookup failed");
            Ok(None)
        }
    }
}

async fn build_failed(
    ctx: &DbContext,
    derivation_build: DerivationBuildId,
    error: &str,
    log_banner: &str,
    kind: BuildFailureKind,
    missing_paths: &[String],
) -> Result<()> {
    let Some(anchor) = EDerivationBuild::find_by_id(derivation_build)
        .one(&ctx.worker_db)
        .await?
    else {
        warn!(%derivation_build, "anchor not found on job_failed");
        return Ok(());
    };

    // Without this banner a pre-`nix build` abort renders as a Failed badge over
    // an empty log.
    if let Some(attempt_id) = gradient_db::latest_attempt_id(&ctx.worker_db, anchor.id)
        .await
        .ok()
        .flatten()
        && let Err(e) = ctx
            .storage
            .log_storage
            .append(
                attempt_id,
                &format!("\n=== build failed: {log_banner} ===\n"),
            )
            .await
    {
        warn!(%derivation_build, error = %e, "failed to append worker error to build log");
    }

    let derivation_id = anchor.derivation;
    let attempt = anchor.attempt;
    let max_attempts = ctx.config.eval.build_max_attempts;

    // Counted before this failure is recorded, so the breaker decision excludes
    // the attempt we are about to mark.
    let prior_inputs_unavailable = if matches!(kind, BuildFailureKind::InputsUnavailable) {
        gradient_db::inputs_unavailable_attempt_count(&ctx.worker_db, derivation_build)
            .await
            .unwrap_or(0)
    } else {
        0
    };

    if let Err(e) = fail_latest_attempt(
        &ctx.worker_db,
        derivation_build,
        policy::attempt_outcome(kind),
        policy::attempt_reason(kind),
        Some(policy::truncate_failure_message(error)),
    )
    .await
    {
        warn!(%derivation_build, error = %e, "failed to record attempt failure reason");
    }

    let max_loops = ctx.config.eval.inputs_unavailable_max_loops;
    let inputs_circuit_open = matches!(kind, BuildFailureKind::InputsUnavailable)
        && policy::inputs_unavailable_circuit_open(prior_inputs_unavailable, max_loops);
    if matches!(kind, BuildFailureKind::InputsUnavailable) && !missing_paths.is_empty() {
        if inputs_circuit_open {
            warn!(
                %derivation_build,
                prior_failures = prior_inputs_unavailable,
                max_loops,
                "InputsUnavailable self-heal circuit open; failing without reconcile to break the hot loop"
            );
        } else if let Err(e) =
            crate::self_heal::reconcile_missing_inputs(ctx, derivation_id, missing_paths).await
        {
            warn!(%derivation_build, error = %e, "failed to reconcile missing inputs");
        }
    }

    // `InputsUnavailable` retries in-eval (the self-heal re-queues its input),
    // but once the breaker trips the input is unrecoverable - stop retrying.
    let outcome = match policy::decide_failure_outcome(kind, attempt, max_attempts) {
        FailureOutcome::Retry if inputs_circuit_open => FailureOutcome::Permanent,
        other => other,
    };
    match outcome {
        FailureOutcome::Retry => {
            let mut active: ADerivationBuild = anchor.clone().into_active_model();
            active.attempt = Set(attempt + 1);
            if let Err(e) = active.update(&ctx.worker_db).await {
                error!(%derivation_build, error = %e, "failed to bump anchor attempt");
            }

            let reloaded = EDerivationBuild::find_by_id(derivation_build)
                .one(&ctx.worker_db)
                .await?
                .unwrap_or(anchor);
            update_derivation_build_status(ctx, reloaded, BuildStatus::FailedTransient).await;
            info!(%derivation_build, attempt = attempt + 1, "transient build failure; scheduled for retry");
            return Ok(());
        }
        FailureOutcome::Requeue => {
            // Substitute miss: back to the queue without an `attempt` bump or a
            // permanent mark, and no dependency cascade - nothing failed.
            update_derivation_build_status(ctx, anchor, BuildStatus::Queued).await;
            info!(%derivation_build, "substitute unavailable; re-queued for re-dispatch/escalation");
            return Ok(());
        }
        FailureOutcome::Aborted => {
            update_derivation_build_status(ctx, anchor, BuildStatus::Aborted).await;
            info!(%derivation_build, "build aborted by server; anchor left requeueable");
            return check_referencing_evals_done(ctx, derivation_id).await;
        }
        FailureOutcome::Permanent => {
            update_derivation_build_status(ctx, anchor, BuildStatus::FailedPermanent).await;
        }
        FailureOutcome::Timeout => {
            update_derivation_build_status(ctx, anchor, BuildStatus::FailedTimeout).await;
        }
    }

    cascade_dependency_failed(&ctx.worker_db, derivation_id).await?;
    check_referencing_evals_done(ctx, derivation_id).await
}

/// After an anchor reaches a terminal status, sweep every evaluation that
/// references the derivation and finalize the settled ones. Idempotent
/// belt-and-braces around the emitter's own finalize (which is skipped when
/// the state machine rejects a racing transition).
async fn check_referencing_evals_done(ctx: &DbContext, derivation: DerivationId) -> Result<()> {
    gradient_db::finalize_evals_for_derivations(ctx, &[derivation]).await?;
    Ok(())
}

/// Insert a `derivation_metric` history row from a build's worker metrics.
async fn record_metrics(
    ctx: &DbContext,
    anchor: &MDerivationBuild,
    derivation_id: DerivationId,
    metrics: &BuildMetrics,
) {
    let (pname, closure_size) = match EDerivation::find_by_id(derivation_id)
        .one(&ctx.worker_db)
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
        worker_id: gradient_db::latest_attempt_worker(&ctx.worker_db, anchor.id)
            .await
            .ok()
            .flatten()
            .unwrap_or_default(),
        created_at: now(),
    }
    .into_active_model();

    if let Err(e) = metric.insert(&ctx.worker_db).await {
        warn!(%derivation_id, error = %e, "failed to record derivation_metric");
    }
}

/// Open the `build_attempt` for a job that just left for a worker and stamp the
/// anchor's `dispatched_at`. Best-effort: failures are logged so instrumentation
/// can't break dispatch.
async fn dispatched(
    ctx: &DbContext,
    evaluation: EvaluationId,
    derivation_build: DerivationBuildId,
    dispatched_job: DispatchedJobId,
    substitute: bool,
    build_context: serde_json::Value,
) {
    if let Some(build_job) = find_or_create_build_job(ctx, evaluation, derivation_build).await
        && let Err(e) = gradient_db::open_attempt(
            &ctx.worker_db,
            build_job,
            derivation_build,
            dispatched_job,
            substitute,
            build_context,
        )
        .await
    {
        warn!(error = %e, "failed to open build_attempt");
    }

    if let Err(e) = EDerivationBuild::update_many()
        .col_expr(CDerivationBuild::DispatchedAt, Expr::value(now()))
        .filter(CDerivationBuild::Id.eq(derivation_build))
        .filter(CDerivationBuild::DispatchedAt.is_null())
        .exec(&ctx.worker_db)
        .await
    {
        warn!(error = %e, %derivation_build, "failed to stamp anchor dispatched_at");
    }
}

/// The `build_job` for `(evaluation, anchor.derivation)`. Ingest normally
/// pre-creates it, so this upserts then selects to stay correct for any anchor
/// whose build_job is missing.
async fn find_or_create_build_job(
    ctx: &DbContext,
    evaluation: EvaluationId,
    derivation_build: DerivationBuildId,
) -> Option<BuildJobId> {
    let anchor = match EDerivationBuild::find_by_id(derivation_build)
        .one(&ctx.worker_db)
        .await
    {
        Ok(Some(a)) => a,
        Ok(None) => {
            warn!(%derivation_build, "anchor missing while opening build_attempt");
            return None;
        }
        Err(e) => {
            warn!(error = %e, %derivation_build, "anchor lookup failed while opening build_attempt");
            return None;
        }
    };

    let existing = EBuildJob::find()
        .filter(CBuildJob::Evaluation.eq(evaluation))
        .filter(CBuildJob::Derivation.eq(anchor.derivation))
        .one(&ctx.worker_db)
        .await;
    match existing {
        Ok(Some(j)) => return Some(j.id),
        Ok(None) => {}
        Err(e) => warn!(error = %e, "build_job lookup failed"),
    }

    let row = MBuildJob {
        id: BuildJobId::now_v7(),
        evaluation,
        derivation: anchor.derivation,
        derivation_build,
        score: 0.0,
        score_breakdown: serde_json::Value::Null,
        created_at: now(),
    }
    .into_active_model();
    if let Err(e) = EBuildJob::insert(row)
        .on_conflict(
            OnConflict::columns([CBuildJob::Evaluation, CBuildJob::Derivation])
                .do_nothing()
                .to_owned(),
        )
        .exec_without_returning(&ctx.worker_db)
        .await
    {
        warn!(error = %e, "build_job upsert failed");
    }

    match EBuildJob::find()
        .filter(CBuildJob::Evaluation.eq(evaluation))
        .filter(CBuildJob::Derivation.eq(anchor.derivation))
        .one(&ctx.worker_db)
        .await
    {
        Ok(j) => j.map(|j| j.id),
        Err(e) => {
            warn!(error = %e, "build_job re-select failed");
            None
        }
    }
}

/// Stamp `ready_at` the first time an anchor became dispatchable, and persist
/// the closure sizes the dispatch pass computed on the way.
async fn ready(
    ctx: &DbContext,
    anchors: &[DerivationBuildId],
    closure_sizes: &[(DerivationId, i64)],
) -> Result<()> {
    let db = &ctx.worker_db;
    gradient_db::for_each_chunk(anchors, |chunk| async move {
        EDerivationBuild::update_many()
            .col_expr(CDerivationBuild::ReadyAt, Expr::value(now()))
            .filter(CDerivationBuild::Id.is_in(chunk))
            .filter(CDerivationBuild::ReadyAt.is_null())
            .exec(db)
            .await
    })
    .await?;

    for (derivation, size) in closure_sizes {
        EDerivation::update_many()
            .col_expr(CDerivation::ClosureSize, Expr::value(*size))
            .filter(CDerivation::Id.eq(*derivation))
            .exec(db)
            .await?;
    }

    Ok(())
}
