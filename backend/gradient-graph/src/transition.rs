/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_db::{
    DbContext,
    graph::{can_start::unpromote_ungated, promotion::cascade_dependency_failed},
    scheduling::build_attempt::{
        abort_running_attempts, fail_latest_attempt, open_failed_attempt, succeed_latest_attempt,
        worker_loss_streaks,
    },
    status::{
        EvaluationFailure, EvaluationWarning, emit_transition_effects,
        update_derivation_build_status, update_evaluation_status,
        update_evaluation_status_with_error,
    },
};
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use gradient_wire::BuildOutputMetadata;
use gradient_wire::types::{BuildFailureKind, BuildMetrics, BuildOutput, BuildProduct};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel,
    QueryFilter,
};
use tracing::{error, info, warn};

use crate::exec;
use crate::messages::{OrphanCause, RamShortage, SubstituteLog, Transition, TransitionReport};
use crate::policy::{self, FailureOutcome, OrphanedBuild};

pub(crate) async fn apply(ctx: &DbContext, transition: Transition) -> Result<TransitionReport> {
    match transition {
        Transition::EvalStreamCompleted { evaluation } => {
            eval_stream_completed(ctx, evaluation).await?;
            Ok(TransitionReport::default())
        }
        Transition::EvalFailed {
            evaluation,
            error,
            kind,
            missing_paths,
        } => {
            eval_failed(ctx, evaluation, &error, kind, &missing_paths).await?;
            Ok(TransitionReport::default())
        }
        Transition::BuildStarted {
            shared_build,
            dispatched_job,
            substitute,
            build_context,
        } => {
            let start = BuildStart {
                dispatched_job,
                substitute,
                build_context,
            };
            build_started(ctx, shared_build, start).await
        }
        Transition::BuildOutput {
            shared_build,
            outputs,
            metrics,
            substituted,
        } => {
            build_output(ctx, shared_build, outputs, metrics, substituted).await?;
            Ok(TransitionReport::default())
        }
        Transition::BuildCompleted { shared_build } => Ok(TransitionReport {
            substitute_log: build_completed(ctx, shared_build).await?,
            ..Default::default()
        }),
        Transition::BuildFailed {
            shared_build,
            error,
            log_banner,
            kind,
            missing_paths,
            metrics,
        } => {
            build_failed(
                ctx,
                shared_build,
                &error,
                &log_banner,
                kind,
                &missing_paths,
                metrics,
            )
            .await?;
            Ok(TransitionReport::default())
        }
        Transition::Assigned { shared_build } => {
            stamp_dispatched_at(ctx, shared_build).await;
            Ok(TransitionReport::default())
        }
        Transition::OrphanedBuilds {
            shared_builds,
            cause,
            observed_peak_ram_mb,
        } => {
            orphaned_builds(ctx, shared_builds, cause, &observed_peak_ram_mb).await?;
            Ok(TransitionReport::default())
        }
        Transition::Ready {
            shared_builds,
            closure_sizes,
        } => {
            ready(ctx, &shared_builds, &closure_sizes).await?;
            Ok(TransitionReport::default())
        }
        Transition::Repair { scope } => {
            gradient_db::graph::repair::repair_build_graph(ctx, scope).await?;
            Ok(TransitionReport::default())
        }
        Transition::AbortEvaluationSharedBuilds { evaluation, hard } => {
            let Some(eval) = EEvaluation::find_by_id(evaluation)
                .one(&ctx.worker_db)
                .await?
            else {
                return Ok(TransitionReport::default());
            };

            let aborted_shared_builds = if hard {
                gradient_db::status::abort_eval_shared_builds(ctx, &eval).await?
            } else {
                Vec::new()
            };
            gradient_db::status::release_evaluation_need(ctx, eval.id).await?;
            Ok(TransitionReport {
                aborted_shared_builds,
                aborted_exec_jobs: exec::abort_evaluation(ctx, eval.id).await?,
                ..Default::default()
            })
        }
        Transition::PrioritizeEvaluation { evaluation } => Ok(TransitionReport {
            prioritized_shared_builds: gradient_db::scheduling::priority::prioritize_evaluation(
                ctx, evaluation,
            )
            .await?,
            ..Default::default()
        }),
        Transition::RequeueImports {
            evaluation,
            derivations,
            without_output_paths,
        } => {
            requeue_imports(ctx, evaluation, &derivations, &without_output_paths).await?;
            Ok(TransitionReport::default())
        }
        Transition::AbortBuild {
            evaluation,
            shared_build,
        } => {
            let Some((eval, row)) = load_evaluation_build(ctx, evaluation, shared_build).await?
            else {
                return Ok(TransitionReport::default());
            };

            Ok(
                match gradient_db::status::abort_evaluation_build(ctx, &eval, &row).await? {
                    Ok(()) => TransitionReport {
                        aborted_shared_builds: vec![row.id],
                        ..Default::default()
                    },
                    Err(refusal) => TransitionReport::from(refusal),
                },
            )
        }
        Transition::RetryBuild {
            evaluation,
            shared_build,
        } => {
            let Some((eval, row)) = load_evaluation_build(ctx, evaluation, shared_build).await?
            else {
                return Ok(TransitionReport::default());
            };

            Ok(
                match gradient_db::status::retry_evaluation_build(ctx, &eval, &row).await? {
                    Ok(()) => TransitionReport::default(),
                    Err(refusal) => TransitionReport::from(refusal),
                },
            )
        }
        Transition::NotEnoughResources { shortages } => {
            not_enough_resources(ctx, &shortages).await?;
            Ok(TransitionReport::default())
        }
        Transition::SettleExecJobs => {
            exec::settle(ctx).await?;
            Ok(TransitionReport::default())
        }
        Transition::ExecJobAssigned {
            exec_job,
            attempt,
            number,
            assignment,
            worker,
            retries_cap,
        } => {
            let assignment = exec::Assignment {
                exec_job,
                attempt,
                number,
                assignment,
                worker,
                retries_cap,
            };
            Ok(TransitionReport {
                already_aborted: !exec::assigned(ctx, assignment).await?,
                ..Default::default()
            })
        }
        Transition::ExecProgramStarted { attempt } => {
            exec::program_started(ctx, attempt).await?;
            Ok(TransitionReport::default())
        }
        Transition::ExecPhase {
            attempt,
            name,
            payload,
        } => {
            exec::phase(ctx, attempt, name, payload).await?;
            Ok(TransitionReport::default())
        }
        Transition::ExecAttemptEnded {
            attempt,
            reason,
            message,
        } => {
            exec::attempt_ended(ctx, attempt, reason, message).await?;
            Ok(TransitionReport::default())
        }
        Transition::AbortExecJob { exec_job } => Ok(TransitionReport {
            aborted_exec_jobs: if exec::abort(ctx, exec_job).await? {
                vec![exec_job]
            } else {
                Vec::new()
            },
            ..Default::default()
        }),
        Transition::PrioritizeBuild { shared_build } => {
            let Some(row) = EDerivationBuild::find_by_id(shared_build)
                .one(&ctx.worker_db)
                .await?
            else {
                return Ok(TransitionReport::default());
            };

            Ok(TransitionReport {
                prioritized_shared_builds:
                    gradient_db::scheduling::priority::prioritize_build_closure(ctx, &row).await?,
                ..Default::default()
            })
        }
    }
}

async fn load_evaluation_build(
    ctx: &DbContext,
    evaluation: EvaluationId,
    shared_build: DerivationBuildId,
) -> Result<Option<(MEvaluation, MDerivationBuild)>> {
    let Some(eval) = EEvaluation::find_by_id(evaluation)
        .one(&ctx.worker_db)
        .await?
    else {
        return Ok(None);
    };

    Ok(EDerivationBuild::find_by_id(shared_build)
        .one(&ctx.worker_db)
        .await?
        .map(|row| (eval, row)))
}

#[tracing::instrument(level = "debug", skip_all, fields(eval_id = %evaluation_id))]
async fn eval_stream_completed(ctx: &DbContext, evaluation_id: EvaluationId) -> Result<()> {
    gradient_db::graph::repair::repair_build_graph(
        ctx,
        gradient_db::graph::repair::RepairScope::Eval(evaluation_id),
    )
    .await?;

    if let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&ctx.worker_db)
        .await?
        && matches!(
            eval.status,
            EvaluationStatus::EvaluatingFlake | EvaluationStatus::EvaluatingDerivation
        )
    {
        info!(%evaluation_id, "eval job complete; promoting evaluation to Building");
        update_evaluation_status(ctx, eval, EvaluationStatus::Building).await?;
    }

    gradient_db::exec_jobs::create::for_evaluation(&ctx.worker_db, evaluation_id).await?;
    gradient_db::status::check_evaluation_done(ctx, evaluation_id).await?;
    Ok(())
}

async fn eval_failed(
    ctx: &DbContext,
    evaluation_id: EvaluationId,
    error: &str,
    kind: BuildFailureKind,
    missing_paths: &[String],
) -> Result<()> {
    if kind == BuildFailureKind::CorruptEvalCache
        && let Some(fingerprint) = missing_paths.first()
        && heal_corrupt_eval_cache(ctx, evaluation_id, fingerprint).await?
    {
        return Ok(());
    }

    if kind == BuildFailureKind::Canceled {
        info!(%evaluation_id, "eval job stopped with its worker; re-queued");
        return requeue_evaluation(ctx, evaluation_id).await;
    }

    if kind == BuildFailureKind::Transient {
        let attempts = gradient_db::scheduling::assignment_record::eval_attempts(
            &ctx.worker_db,
            evaluation_id,
        )
        .await?;
        if crate::policy::retry_failed_eval(kind, attempts, ctx.config.build.max_attempts.get()) {
            warn!(%evaluation_id, attempts, %error, "eval job hit an outage; re-queued");
            requeue_evaluation(ctx, evaluation_id).await?;
            return Ok(());
        }
    }

    if let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&ctx.worker_db)
        .await?
        && !eval.status.is_terminal()
    {
        if kind == BuildFailureKind::Aborted {
            update_evaluation_status(ctx, eval, EvaluationStatus::Aborted).await?;
            return Ok(());
        }

        update_evaluation_status_with_error(
            ctx,
            eval,
            EvaluationStatus::Failed,
            &EvaluationFailure::ReportedByWorker(error),
        )
        .await?;
    }

    Ok(())
}

const HEAL_WINDOW: chrono::Duration = chrono::Duration::hours(1);

/// A purge within the last `HEAL_WINDOW` is acting as the circuit breaker: workers push segments
/// during the requeued evaluation, so a recurring corruption fails instead of purging in a loop.
async fn heal_corrupt_eval_cache(
    ctx: &DbContext,
    evaluation_id: EvaluationId,
    fingerprint: &str,
) -> Result<bool> {
    let rows = EEvalCacheStore::find()
        .filter(CEvalCacheStore::Fingerprint.eq(fingerprint))
        .all(&ctx.worker_db)
        .await?;
    let now = gradient_types::now();
    if rows.is_empty() || purged_recently(&rows, now) {
        warn!(%evaluation_id, %fingerprint, "corrupt eval-cache recurred after a recent purge; failing eval");
        return Ok(false);
    }

    EEvalCacheStore::update_many()
        .col_expr(CEvalCacheStore::PurgedAt, Expr::value(now))
        .col_expr(CEvalCacheStore::SizeBytes, Expr::value(0i64))
        .filter(CEvalCacheStore::Fingerprint.eq(fingerprint))
        .exec(&ctx.worker_db)
        .await?;
    for row in rows {
        if let Err(e) = ctx
            .storage
            .nar_storage
            .delete_eval_cache_segments(row.cache, &row.fingerprint)
            .await
        {
            warn!(key = %row.storage_path, error = %e, "failed to delete corrupt eval-cache segments");
        }
    }

    requeue_evaluation(ctx, evaluation_id).await?;
    info!(%evaluation_id, %fingerprint, "purged corrupt eval-cache segments; re-queued eval for fresh evaluation");
    Ok(true)
}

fn purged_recently(rows: &[MEvalCacheStore], now: chrono::NaiveDateTime) -> bool {
    rows.iter()
        .filter_map(|row| row.purged_at)
        .any(|purged_at| purged_at > now - HEAL_WINDOW)
}

async fn requeue_evaluation(ctx: &DbContext, evaluation_id: EvaluationId) -> Result<()> {
    if let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&ctx.worker_db)
        .await?
        && !eval.status.is_terminal()
    {
        update_evaluation_status(ctx, eval, EvaluationStatus::Queued).await?;
    }

    Ok(())
}

async fn build_output(
    ctx: &DbContext,
    derivation_build: DerivationBuildId,
    outputs: Vec<BuildOutput>,
    metrics: Option<BuildMetrics>,
    substituted: bool,
) -> Result<()> {
    let shared_build = EDerivationBuild::find_by_id(derivation_build)
        .one(&ctx.worker_db)
        .await
        .context("fetch derivation_build")?
        .with_context(|| format!("derivation_build {derivation_build} not found"))?;

    let build_id = shared_build.id;
    let derivation_id = shared_build.derivation;
    let end = if substituted {
        policy::BuildEnd::Substituted
    } else {
        policy::BuildEnd::Built
    };
    if let Some(metrics) = policy::history_sample(metrics, end) {
        record_metrics(ctx, &shared_build, derivation_id, &metrics).await;
    }

    let mut missing: Vec<&BuildProduct> = Vec::new();
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
        let unrealised = row.hash.is_none();
        let mut active = row.into_active_model();
        if unrealised {
            record_realised_path(&mut active, output);
        }

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

        if let Err(e) = EBuildProduct::delete_many()
            .filter(CBuildProduct::DerivationOutput.eq(row_id))
            .exec(&ctx.worker_db)
            .await
            .context("delete prior build_product rows")
        {
            warn!(error = %e, %build_id, output_name = %output.name, "failed to delete prior build_product rows");
        }

        let (present, absent): (Vec<&BuildProduct>, Vec<&BuildProduct>) =
            output.products.iter().partition(|p| p.size.is_some());
        missing.extend(absent);

        for product in present {
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
    report_missing_artefacts(ctx, derivation_id, &missing).await?;

    if substituted {
        let mut active = shared_build.into_active_model();
        active.substituted = Set(true);
        active.updated_at = Set(now());
        if let Err(e) = active.update(&ctx.worker_db).await {
            warn!(%build_id, error = %e, "failed to record shared build as substituted");
        }
    }

    Ok(())
}

fn record_realised_path(active: &mut ADerivationOutput, output: &BuildOutput) {
    match StorePath::parse_stored(&output.store_path) {
        Ok(path) => {
            active.hash = Set(Some(path.hash().to_owned()));
            active.package = Set(Some(path.name().to_owned()));
        }
        Err(e) => {
            warn!(store_path = %output.store_path, error = %e, "the realised output path does not parse");
        }
    }
}

async fn report_missing_artefacts(
    ctx: &DbContext,
    derivation: DerivationId,
    missing: &[&BuildProduct],
) -> Result<()> {
    if missing.is_empty() {
        return Ok(());
    }

    let referencing = gradient_db::graph::reachability::evals_referencing_derivations(
        &ctx.worker_db,
        &[derivation],
    )
    .await?;
    let active = EEvaluation::find()
        .filter(CEvaluation::Id.is_in(referencing))
        .filter(CEvaluation::Status.is_in(EvaluationStatus::ACTIVE))
        .all(&ctx.worker_db)
        .await
        .context("fetch active evaluations for missing artefacts")?;

    for evaluation in &active {
        for product in missing {
            EvaluationWarning::ArtefactMissing {
                path: &product.path,
                file_type: &product.file_type,
                subtype: &product.subtype,
            }
            .report(&ctx.worker_db, evaluation.id)
            .await;
        }
    }
    Ok(())
}

async fn build_completed(
    ctx: &DbContext,
    derivation_build: DerivationBuildId,
) -> Result<Option<SubstituteLog>> {
    let Some(shared_build) = EDerivationBuild::find_by_id(derivation_build)
        .one(&ctx.worker_db)
        .await?
    else {
        warn!(%derivation_build, "shared build not found on job_completed");
        return Ok(None);
    };

    let derivation_id = shared_build.derivation;
    let was_external_cached = shared_build.cache_available;

    let terminal = policy::terminal_success_status(shared_build.substituted);
    if let Err(e) = succeed_latest_attempt(
        &ctx.worker_db,
        derivation_build,
        policy::terminal_success_outcome(shared_build.substituted),
    )
    .await
    {
        warn!(%derivation_build, error = %e, "failed to record attempt success");
    }
    update_derivation_build_status(ctx, shared_build, terminal).await?;
    check_referencing_evals_done(ctx, derivation_id).await?;

    if !was_external_cached {
        return Ok(None);
    }

    match EDerivation::find_by_id(derivation_id)
        .one(&ctx.worker_db)
        .await
    {
        Ok(Some(d)) => Ok(Some(SubstituteLog {
            shared_build: derivation_build,
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

async fn requeue_imports(
    ctx: &DbContext,
    evaluation: EvaluationId,
    derivations: &[DerivationId],
    without_output_paths: &[DerivationId],
) -> Result<()> {
    let mut changes =
        gradient_db::graph::promotion::requeue_failed_import_closure(&ctx.worker_db, derivations)
            .await
            .context("requeue the failed closure of an import")?;
    changes.extend(
        gradient_db::graph::runtime_can_start::reset_builds_without_output_paths(
            &ctx.worker_db,
            without_output_paths,
        )
        .await
        .context("rebuild the imports without a recorded output path")?,
    );
    emit_transition_effects(ctx, &changes).await?;

    let thawed: Vec<DerivationId> = changes
        .iter()
        .filter(|change| change.to == BuildStatus::Created)
        .map(|change| change.derivation)
        .collect();
    gradient_db::status::wanted_again(ctx, evaluation, &thawed).await?;
    let need =
        gradient_db::graph::can_start::update_and_settle_need(&ctx.worker_db, &thawed).await?;
    emit_transition_effects(ctx, &need.changes).await?;
    let settled = gradient_db::graph::can_start::settle_created(&ctx.worker_db, &thawed).await?;
    emit_transition_effects(ctx, &settled).await?;

    Ok(())
}

async fn build_failed(
    ctx: &DbContext,
    derivation_build: DerivationBuildId,
    error: &str,
    log_banner: &str,
    kind: BuildFailureKind,
    missing_paths: &[String],
    metrics: Option<BuildMetrics>,
) -> Result<()> {
    let Some(shared_build) = EDerivationBuild::find_by_id(derivation_build)
        .one(&ctx.worker_db)
        .await?
    else {
        warn!(%derivation_build, "shared build not found on job_failed");
        return Ok(());
    };

    let out_of_memory = policy::out_of_memory(kind, metrics.as_ref());
    let oom_cores = metrics.as_ref().and_then(|m| m.build_cores);
    let fewest_before = if out_of_memory {
        gradient_db::derivation_dependencies::fewest_out_of_memory_cores(
            &ctx.worker_db,
            shared_build.derivation,
        )
        .await?
    } else {
        None
    };
    let kind = policy::retried_as(kind, out_of_memory);
    if let Some(metrics) = policy::history_sample(metrics, policy::BuildEnd::Failed) {
        record_metrics(ctx, &shared_build, shared_build.derivation, &metrics).await;
    }

    let running_attempt = running_attempt_id(ctx, derivation_build).await;
    if let Some(attempt_id) = running_attempt
        && let Err(e) = ctx
            .storage
            .log_storage
            .append(attempt_id, &policy::failure_log_entry(log_banner))
            .await
    {
        warn!(%derivation_build, error = %e, "failed to append worker error to build log");
    }

    let derivation_id = shared_build.derivation;
    let attempt = shared_build.attempt;
    let max_attempts = ctx.config.build.max_attempts.get();

    let prior_inputs_unavailable = if matches!(kind, BuildFailureKind::InputsUnavailable) {
        gradient_db::scheduling::build_attempt::inputs_unavailable_attempt_count(
            &ctx.worker_db,
            derivation_build,
        )
        .await
        .unwrap_or(0)
    } else {
        0
    };

    let max_loops = ctx.config.build.inputs_unavailable_max_loops.get();
    let inputs_circuit_open = matches!(kind, BuildFailureKind::InputsUnavailable)
        && policy::inputs_unavailable_circuit_open(prior_inputs_unavailable, max_loops);
    if matches!(kind, BuildFailureKind::InputsUnavailable) && !missing_paths.is_empty() {
        if inputs_circuit_open {
            warn!(
                %derivation_build,
                prior_failures = prior_inputs_unavailable,
                max_loops,
                "InputsUnavailable self-heal circuit open; failing without repair to break the hot loop"
            );
        } else if let Err(e) =
            crate::self_heal::repair_missing_inputs(ctx, derivation_id, missing_paths).await
        {
            warn!(%derivation_build, error = %e, "failed to repair missing inputs");
        }
    }

    let substitution = policy::Substitution {
        cache_available: shared_build.cache_available,
        misses: substitute_misses(ctx, derivation_build, kind, shared_build.cache_available).await,
        threshold: i64::from(ctx.config.build.substitute_miss_escalation_threshold.get()),
    };
    let oom_outcome = out_of_memory
        .then(|| policy::out_of_memory_outcome(oom_cores, fewest_before))
        .flatten();
    let outcome = match oom_outcome {
        Some(outcome) => outcome,
        None => match policy::decide_failure_outcome(kind, attempt, max_attempts, substitution) {
            FailureOutcome::Retry if inputs_circuit_open => FailureOutcome::Permanent,
            other => other,
        },
    };
    let error = if oom_outcome == Some(FailureOutcome::Permanent) {
        policy::OUT_OF_MEMORY_AT_ONE_CORE
    } else {
        error
    };

    if running_attempt.is_some()
        && let Err(e) = fail_latest_attempt(
            &ctx.worker_db,
            derivation_build,
            policy::attempt_outcome(kind),
            policy::attempt_reason_for(kind, outcome, out_of_memory),
            Some(policy::truncate_failure_message(error)),
        )
        .await
    {
        warn!(%derivation_build, error = %e, "failed to store the attempt failure reason");
    }

    match outcome {
        FailureOutcome::RetryWithFewerCores => {
            update_derivation_build_status(ctx, shared_build, BuildStatus::FailedTransient).await?;
            info!(%derivation_build, cores = ?oom_cores, "out of memory; retried with fewer cores");
            return Ok(());
        }
        FailureOutcome::Retry => {
            let mut active: ADerivationBuild = shared_build.clone().into_active_model();
            active.attempt = Set(attempt + 1);
            if let Err(e) = active.update(&ctx.worker_db).await {
                error!(%derivation_build, error = %e, "failed to bump shared build attempt");
            }

            let reloaded = EDerivationBuild::find_by_id(derivation_build)
                .one(&ctx.worker_db)
                .await?
                .unwrap_or(shared_build);
            update_derivation_build_status(ctx, reloaded, BuildStatus::FailedTransient).await?;
            info!(%derivation_build, attempt = attempt + 1, "transient build failure; scheduled for retry");
            return Ok(());
        }
        FailureOutcome::Requeue | FailureOutcome::Canceled => {
            // `repair_missing_inputs` above is purging stale cached inputs in this same call.
            // A purged input is raising this shared build's `blocking_deps`.
            // The settle is what makes the `Queued` write legal.
            update_derivation_build_status(ctx, shared_build, BuildStatus::Queued).await?;
            let settled = unpromote_ungated(&ctx.worker_db, &[derivation_id]).await?;
            emit_transition_effects(ctx, &settled).await?;
            info!(%derivation_build, ?outcome, "re-queued for re-dispatch");
            return Ok(());
        }
        FailureOutcome::Exhausted => {
            let misses = substitution.misses + 1;
            exhaust_substitution(ctx, &shared_build, misses).await?;
            info!(%derivation_build, misses, "substitute misses exhausted; the shared build will be built");
            return Ok(());
        }
        FailureOutcome::Aborted => {
            update_derivation_build_status(ctx, shared_build, BuildStatus::Aborted).await?;
            info!(%derivation_build, "build aborted by server; shared build left requeueable");
            return check_referencing_evals_done(ctx, derivation_id).await;
        }
        FailureOutcome::Permanent => {
            update_derivation_build_status(ctx, shared_build, BuildStatus::FailedPermanent).await?;
        }
        FailureOutcome::Timeout => {
            update_derivation_build_status(ctx, shared_build, BuildStatus::FailedTimeout).await?;
        }
    }

    cascade_dependency_failed(&ctx.worker_db, derivation_id).await?;
    check_referencing_evals_done(ctx, derivation_id).await
}

async fn orphaned_builds(
    ctx: &DbContext,
    shared_builds: Vec<DerivationBuildId>,
    cause: OrphanCause,
    observed_peak_ram_mb: &std::collections::HashMap<DerivationBuildId, u64>,
) -> Result<()> {
    let rows = match EDerivationBuild::find()
        .filter(gradient_entity::derivation_build::Column::Id.is_in(shared_builds))
        .all(&ctx.worker_db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "requeue orphaned builds: load failed");
            return Ok(());
        }
    };

    let building: Vec<_> = rows
        .into_iter()
        .filter(|r| r.status == BuildStatus::Building)
        .collect();
    let ids: Vec<DerivationBuildId> = building.iter().map(|r| r.id).collect();
    let losses = match cause {
        OrphanCause::WorkerLost => {
            abort_running_attempts(
                &ctx.worker_db,
                &ids,
                Some(AttemptFailureReason::WorkerLost),
                "the worker disconnected before the build finished",
            )
            .await?;
            for row in &building {
                if let Some(peak) = observed_peak_ram_mb.get(&row.id) {
                    let observed = BuildMetrics {
                        peak_ram_mb: Some(*peak),
                        ..Default::default()
                    };
                    record_metrics(ctx, row, row.derivation, &observed).await;
                }
            }
            worker_loss_streaks(&ctx.worker_db, &ids).await?
        }
        OrphanCause::ClusterAttemptEnded | OrphanCause::AccessRevoked => {
            let message = match cause {
                OrphanCause::AccessRevoked => "the project revoked the worker's access",
                _ => "the cluster attempt ended before the build finished",
            };
            abort_running_attempts(&ctx.worker_db, &ids, None, message).await?;
            Default::default()
        }
    };

    let mut requeued = Vec::new();
    for row in building {
        let lost = losses.get(&row.id).copied().unwrap_or(0);
        match policy::orphaned_build_outcome(lost, policy::MAX_WORKER_LOSSES) {
            OrphanedBuild::Requeue => {
                let derivation = row.derivation;
                update_derivation_build_status(ctx, row, BuildStatus::Queued).await?;
                requeued.push(derivation);
            }
            OrphanedBuild::Exhausted => fail_build_lost_with_every_worker(ctx, row, lost).await?,
        }
    }

    let settled = unpromote_ungated(&ctx.worker_db, &requeued).await?;
    emit_transition_effects(ctx, &settled).await?;
    Ok(())
}

async fn fail_build_lost_with_every_worker(
    ctx: &DbContext,
    shared_build: MDerivationBuild,
    losses: i64,
) -> Result<()> {
    let derivation_build = shared_build.id;
    let derivation = shared_build.derivation;
    let error = format!(
        "the build was dispatched {losses} times in a row and each worker disconnected before \
         reporting a result; giving up"
    );
    warn!(%derivation_build, losses, "build lost with every worker; failing instead of re-queuing");

    if let Some(attempt_id) =
        gradient_db::scheduling::build_attempt::latest_attempt_id(&ctx.worker_db, derivation_build)
            .await
            .ok()
            .flatten()
        && let Err(e) = ctx
            .storage
            .log_storage
            .append(attempt_id, &policy::failure_log_entry(&error))
            .await
    {
        warn!(%derivation_build, error = %e, "failed to append the worker loss to the build log");
    }

    if let Err(e) = fail_latest_attempt(
        &ctx.worker_db,
        derivation_build,
        AttemptOutcome::Failed,
        Some(AttemptFailureReason::WorkerLost),
        Some(error),
    )
    .await
    {
        warn!(%derivation_build, error = %e, "failed to close the last attempt of a lost build");
    }

    update_derivation_build_status(ctx, shared_build, BuildStatus::FailedPermanent).await?;
    cascade_dependency_failed(&ctx.worker_db, derivation).await?;
    check_referencing_evals_done(ctx, derivation).await
}

async fn not_enough_resources(ctx: &DbContext, shortages: &[RamShortage]) -> Result<()> {
    for shortage in shortages {
        if let Some(shared_build) = queued_without_worker(ctx, shortage.shared_build).await? {
            fail_build_no_worker_fits(ctx, shared_build, shortage).await?;
        }
    }

    Ok(())
}

async fn queued_without_worker(
    ctx: &DbContext,
    derivation_build: DerivationBuildId,
) -> Result<Option<MDerivationBuild>> {
    let Some(shared_build) = EDerivationBuild::find_by_id(derivation_build)
        .one(&ctx.worker_db)
        .await?
    else {
        return Ok(None);
    };
    if shared_build.status != BuildStatus::Queued {
        return Ok(None);
    }

    let dispatched = gradient_db::scheduling::assignment_record::build_has_open_assignment(
        &ctx.worker_db,
        derivation_build,
    )
    .await?;

    Ok((!dispatched).then_some(shared_build))
}

async fn fail_build_no_worker_fits(
    ctx: &DbContext,
    shared_build: MDerivationBuild,
    shortage: &RamShortage,
) -> Result<()> {
    let derivation_build = shared_build.id;
    let derivation = shared_build.derivation;
    let error =
        policy::not_enough_resources_message(shortage.needed_mb, shortage.largest_available_mb);
    warn!(
        %derivation_build,
        needed_mb = shortage.needed_mb,
        largest_available_mb = shortage.largest_available_mb,
        "build fits on no connected worker; failing instead of leaving it queued"
    );

    open_shortage_attempt(ctx, &shared_build, shortage, &error).await;
    update_derivation_build_status(ctx, shared_build, BuildStatus::FailedPermanent).await?;
    cascade_dependency_failed(&ctx.worker_db, derivation).await?;
    check_referencing_evals_done(ctx, derivation).await
}

async fn open_shortage_attempt(
    ctx: &DbContext,
    shared_build: &MDerivationBuild,
    shortage: &RamShortage,
    error: &str,
) {
    let derivation_build = shared_build.id;
    let Some(build_job) = find_or_create_build_job(ctx, shortage.evaluation, shared_build).await
    else {
        return;
    };

    let superseded =
        gradient_db::scheduling::build_attempt::latest_attempt_id(&ctx.worker_db, derivation_build)
            .await;
    let attempt = match open_failed_attempt(
        &ctx.worker_db,
        build_job,
        derivation_build,
        AttemptFailureReason::NotEnoughResources,
        error.to_owned(),
    )
    .await
    {
        Ok(attempt) => attempt,
        Err(e) => {
            warn!(%derivation_build, error = %e, "failed to open the attempt of a build no worker fits");
            return;
        }
    };

    finalize_superseded_log(ctx, superseded).await;
    if let Err(e) = ctx
        .storage
        .log_storage
        .append(attempt.id, &policy::failure_log_entry(error))
        .await
    {
        warn!(%derivation_build, error = %e, "failed to append the memory shortage to the build log");
    }
}

async fn substitute_misses(
    ctx: &DbContext,
    derivation_build: DerivationBuildId,
    kind: BuildFailureKind,
    cache_available: bool,
) -> i64 {
    if !cache_available || !policy::spends_substitute_budget(kind) {
        return 0;
    }

    let Ok(Some(evaluation)) = gradient_db::scheduling::build_attempt::latest_attempt_evaluation(
        &ctx.worker_db,
        derivation_build,
    )
    .await
    else {
        return 0;
    };

    gradient_db::scheduling::build_attempt::substitute_miss_counts(
        &ctx.worker_db,
        &[derivation_build],
    )
    .await
    .unwrap_or_default()
    .get(&(derivation_build, evaluation))
    .copied()
    .unwrap_or(0)
}

fn clear_shared_build_substitution_sql() -> String {
    format!(
        "UPDATE derivation_build SET cache_available = false, status = {created}, attempt = 0, \
         updated_at = (now() AT TIME ZONE 'UTC') WHERE id = $1 AND status IN ({unfinished})",
        created = gradient_db::sql::status::to(BuildStatus::BUILD_AFTER_SUBSTITUTE_MISSES),
        unfinished = gradient_db::sql::status::from(BuildStatus::BUILD_AFTER_SUBSTITUTE_MISSES),
    )
}

gradient_db::sql_fn! {
    CLEAR_SHARED_BUILD_SUBSTITUTION = clear_shared_build_substitution_sql,
        params = [SharedBuildId],
        status_write = gradient_db::sql::StatusWrite::build(BuildStatus::BUILD_AFTER_SUBSTITUTE_MISSES);
}

gradient_db::sql! {
    CLEAR_OUTPUTS_UPSTREAM_RECORD = "UPDATE derivation_output SET external_url = NULL, nar_hash = NULL, file_hash = NULL, \
         file_size = NULL, nar_size = NULL, references_list = NULL, deriver = NULL \
         WHERE derivation = $1",
        params = [DerivationId];
}

async fn exhaust_substitution(
    ctx: &DbContext,
    shared_build: &MDerivationBuild,
    misses: i64,
) -> Result<()> {
    let db = &ctx.worker_db;
    let cleared = db
        .execute_raw(CLEAR_SHARED_BUILD_SUBSTITUTION.bind([shared_build.id.into_inner().into()]))
        .await
        .context("clear the shared build's substitution")?;
    if cleared.rows_affected() == 0 {
        info!(derivation_build = %shared_build.id, "the shared build finished before its substitute misses were exhausted");
        return Ok(());
    }
    db.execute_raw(
        CLEAR_OUTPUTS_UPSTREAM_RECORD.bind([shared_build.derivation.into_inner().into()]),
    )
    .await
    .context("clear the outputs' upstream record")?;

    let mut changes = vec![gradient_db::status::TransitionChange {
        derivation: shared_build.derivation,
        from: shared_build.status,
        to: BuildStatus::Created,
    }];
    changes.extend(
        gradient_db::graph::can_start::update_and_settle_need(db, &[shared_build.derivation])
            .await?
            .changes,
    );
    changes.extend(
        gradient_db::graph::can_start::settle_created(db, &[shared_build.derivation]).await?,
    );
    emit_transition_effects(ctx, &changes).await?;

    if let Ok(Some(drv)) = EDerivation::find_by_id(shared_build.derivation)
        .one(db)
        .await
    {
        let jobs = gradient_db::graph::reachability::build_jobs_for_derivations(
            db,
            &[shared_build.derivation],
        )
        .await?;
        let store_path = drv.store_path();
        for job in jobs.values().flatten() {
            EvaluationWarning::SubstitutionExhausted {
                store_path: &store_path,
                misses,
            }
            .insert(db, job.evaluation)
            .await?;
        }
    }

    Ok(())
}

async fn check_referencing_evals_done(ctx: &DbContext, derivation: DerivationId) -> Result<()> {
    gradient_db::status::finalize_evals_for_derivations(ctx, &[derivation]).await?;
    Ok(())
}

async fn record_metrics(
    ctx: &DbContext,
    shared_build: &MDerivationBuild,
    derivation_id: DerivationId,
    metrics: &BuildMetrics,
) {
    let derivation = match EDerivation::find_by_id(derivation_id)
        .one(&ctx.worker_db)
        .await
    {
        Ok(Some(d)) => d,
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
        pname: Some(derivation.history_name().to_owned()),
        architecture: derivation.architecture,
        closure_size: derivation.closure_size,
        peak_ram_mb: metrics.peak_ram_mb.map(|v| v as i64),
        cpu_time_ms: metrics.cpu_time_ms.map(|v| v as i64),
        avg_cpu_pct: metrics.avg_cpu_pct.map(|v| v as f64),
        disk_read_bytes: metrics.disk_read_bytes.map(|v| v as i64),
        disk_write_bytes: metrics.disk_write_bytes.map(|v| v as i64),
        oom_killed: metrics.oom_killed,
        build_time_ms: metrics.build_time_ms.map(|v| v as i64),
        concurrent_builds: metrics.concurrent_builds.map(|v| v as i32),
        build_cores: metrics.build_cores.map(|v| v as i32),
        cpu_core_score: metrics.cpu_core_score.map(|v| v as i32),
        input_pnames: Some(
            gradient_db::derivation_dependencies::build_input_pnames(&ctx.worker_db, derivation_id)
                .await
                .unwrap_or_default(),
        ),
        worker_id: gradient_db::scheduling::build_attempt::latest_attempt_worker(
            &ctx.worker_db,
            shared_build.id,
        )
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

struct BuildStart {
    dispatched_job: DispatchedJobId,
    substitute: bool,
    build_context: serde_json::Value,
}

async fn build_started(
    ctx: &DbContext,
    derivation_build: DerivationBuildId,
    start: BuildStart,
) -> Result<TransitionReport> {
    let Some(row) = EDerivationBuild::find_by_id(derivation_build)
        .one(&ctx.worker_db)
        .await?
    else {
        warn!(%derivation_build, "shared build not found for Building status update");
        return Ok(TransitionReport::default());
    };

    match row.status {
        BuildStatus::Aborted => {
            return Ok(TransitionReport {
                already_aborted: true,
                ..Default::default()
            });
        }
        BuildStatus::Queued => open_started_attempt(ctx, &row, start).await,
        _ => {}
    }

    update_derivation_build_status(ctx, row, BuildStatus::Building).await?;
    Ok(TransitionReport::default())
}

async fn open_started_attempt(ctx: &DbContext, shared_build: &MDerivationBuild, start: BuildStart) {
    let Some(evaluation) = dispatch_evaluation(ctx, start.dispatched_job).await else {
        return;
    };
    let Some(build_job) = find_or_create_build_job(ctx, evaluation, shared_build).await else {
        return;
    };

    let superseded =
        gradient_db::scheduling::build_attempt::latest_attempt_id(&ctx.worker_db, shared_build.id)
            .await;
    match gradient_db::scheduling::build_attempt::open_attempt(
        &ctx.worker_db,
        build_job,
        shared_build.id,
        start.dispatched_job,
        start.substitute,
        start.build_context,
    )
    .await
    {
        Ok(_) => finalize_superseded_log(ctx, superseded).await,
        Err(e) => warn!(error = %e, "failed to open build_attempt"),
    }
}

async fn dispatch_evaluation(
    ctx: &DbContext,
    dispatched_job: DispatchedJobId,
) -> Option<EvaluationId> {
    match gradient_entity::dispatched_job::Entity::find_by_id(dispatched_job)
        .one(&ctx.worker_db)
        .await
    {
        Ok(Some(dispatch)) => Some(dispatch.evaluation_id),
        Ok(None) => {
            warn!(%dispatched_job, "dispatch row missing while opening build_attempt");
            None
        }
        Err(e) => {
            warn!(error = %e, %dispatched_job, "dispatch row lookup failed while opening build_attempt");
            None
        }
    }
}

async fn running_attempt_id(
    ctx: &DbContext,
    derivation_build: DerivationBuildId,
) -> Option<BuildAttemptId> {
    gradient_db::scheduling::build_attempt::running_attempt_id(&ctx.worker_db, derivation_build)
        .await
        .ok()
        .flatten()
}

async fn stamp_dispatched_at(ctx: &DbContext, derivation_build: DerivationBuildId) {
    if let Err(e) = EDerivationBuild::update_many()
        .col_expr(CDerivationBuild::DispatchedAt, Expr::value(now()))
        .filter(CDerivationBuild::Id.eq(derivation_build))
        .filter(CDerivationBuild::DispatchedAt.is_null())
        .exec(&ctx.worker_db)
        .await
    {
        warn!(error = %e, %derivation_build, "failed to stamp shared build dispatched_at");
    }
}

async fn finalize_superseded_log(
    ctx: &DbContext,
    superseded: Result<Option<BuildAttemptId>, sea_orm::DbErr>,
) {
    let result = match superseded {
        Ok(attempt) => gradient_db::status::enqueue_log_finalize(&ctx.worker_db, attempt).await,
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        warn!(error = %e, "failed to finalize the superseded attempt's log");
    }
}

async fn find_or_create_build_job(
    ctx: &DbContext,
    evaluation: EvaluationId,
    shared_build: &MDerivationBuild,
) -> Option<BuildJobId> {
    let existing = EBuildJob::find()
        .filter(CBuildJob::Evaluation.eq(evaluation))
        .filter(CBuildJob::Derivation.eq(shared_build.derivation))
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
        derivation: shared_build.derivation,
        derivation_build: shared_build.id,
        created_at: now(),
        ..Default::default()
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
        .filter(CBuildJob::Derivation.eq(shared_build.derivation))
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

gradient_db::sql! {
    SET_CLOSURE_SIZES = "UPDATE derivation SET closure_size = v.size \
             FROM (SELECT unnest($1::uuid[]) AS id, unnest($2::bigint[]) AS size) v \
             WHERE derivation.id = v.id",
        params = [DerivationIds(64), Ints(1200, 64)];
}

async fn ready(
    ctx: &DbContext,
    shared_builds: &[DerivationBuildId],
    closure_sizes: &[(DerivationId, i64)],
) -> Result<()> {
    let db = &ctx.worker_db;
    gradient_db::for_each_chunk(shared_builds, |chunk| async move {
        EDerivationBuild::update_many()
            .col_expr(CDerivationBuild::ReadyAt, Expr::value(now()))
            .filter(CDerivationBuild::Id.is_in(chunk))
            .filter(CDerivationBuild::ReadyAt.is_null())
            .exec(db)
            .await
    })
    .await?;

    gradient_db::for_each_chunk(closure_sizes, |chunk| async move {
        let (ids, sizes): (Vec<uuid::Uuid>, Vec<i64>) = chunk
            .iter()
            .map(|(derivation, size)| (uuid::Uuid::from(*derivation), *size))
            .unzip();

        db.execute_raw(SET_CLOSURE_SIZES.bind([ids.into(), sizes.into()]))
            .await
    })
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_status_write_declares_its_transition() {
        let violations = gradient_db::sql::status_write::violations();
        assert!(violations.is_empty(), "{}", violations.join("\n"));
    }
}
