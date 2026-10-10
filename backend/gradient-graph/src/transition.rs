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
        abort_running_attempts, fail_latest_attempt, succeed_latest_attempt,
    },
    status::{
        emit_transition_effects, update_derivation_build_status, update_evaluation_status,
        update_evaluation_status_with_error,
    },
};
use gradient_entity::build::BuildStatus;
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

use crate::messages::{SubstituteLog, Transition, TransitionReport};
use crate::policy::{self, FailureOutcome};

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
        Transition::BuildStarted { shared_build } => {
            let Some(row) = EDerivationBuild::find_by_id(shared_build)
                .one(&ctx.worker_db)
                .await?
            else {
                warn!(derivation_build = %shared_build, "shared build not found for Building status update");
                return Ok(TransitionReport::default());
            };

            if row.status == BuildStatus::Aborted {
                return Ok(TransitionReport {
                    already_aborted: true,
                    ..Default::default()
                });
            }

            update_derivation_build_status(ctx, row, BuildStatus::Building).await?;
            Ok(TransitionReport::default())
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
        Transition::Assigned {
            evaluation,
            shared_build,
            dispatched_job,
            substitute,
            build_context,
        } => {
            assigned(
                ctx,
                evaluation,
                shared_build,
                dispatched_job,
                substitute,
                build_context,
            )
            .await;
            Ok(TransitionReport::default())
        }
        Transition::OrphanedBuilds { shared_builds } => {
            let rows = match EDerivationBuild::find()
                .filter(gradient_entity::derivation_build::Column::Id.is_in(shared_builds))
                .all(&ctx.worker_db)
                .await
            {
                Ok(rows) => rows,
                Err(e) => {
                    warn!(error = %e, "requeue orphaned builds: load failed");
                    return Ok(TransitionReport::default());
                }
            };

            let building: Vec<_> = rows
                .into_iter()
                .filter(|r| r.status == BuildStatus::Building)
                .collect();
            let ids: Vec<DerivationBuildId> = building.iter().map(|r| r.id).collect();
            abort_running_attempts(
                &ctx.worker_db,
                &ids,
                "the worker disconnected before the build finished",
            )
            .await?;

            let mut requeued = Vec::new();
            for row in building {
                let derivation = row.derivation;
                update_derivation_build_status(ctx, row, BuildStatus::Queued).await?;
                requeued.push(derivation);
            }

            let settled = unpromote_ungated(&ctx.worker_db, &requeued).await?;
            emit_transition_effects(ctx, &settled).await?;

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
        Transition::AbortEvaluationSharedBuilds { evaluation } => {
            let Some(eval) = EEvaluation::find_by_id(evaluation)
                .one(&ctx.worker_db)
                .await?
            else {
                return Ok(TransitionReport::default());
            };

            let aborted_shared_builds =
                gradient_db::status::abort_eval_shared_builds(ctx, &eval).await?;
            gradient_db::status::release_evaluation_need(ctx, eval.id).await?;
            Ok(TransitionReport {
                aborted_shared_builds,
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
        Transition::RequeueImports { derivations } => {
            requeue_imports(ctx, &derivations).await?;
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
                    Err(refusal) => refused(refusal),
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
                    Err(refusal) => refused(refusal),
                },
            )
        }
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

fn refused(refusal: gradient_db::status::BuildRefusal) -> TransitionReport {
    TransitionReport {
        refusal: Some(refusal),
        ..Default::default()
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
        if crate::policy::retry_failed_eval(kind, attempts, ctx.config.build.max_attempts) {
            warn!(%evaluation_id, attempts, %error, "eval job hit an outage; re-queued");
            requeue_evaluation(ctx, evaluation_id).await?;
            return Ok(());
        }
    }

    if let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&ctx.worker_db)
        .await?
        && !matches!(
            eval.status,
            EvaluationStatus::Completed | EvaluationStatus::Failed | EvaluationStatus::Aborted
        )
    {
        if kind == BuildFailureKind::Aborted {
            update_evaluation_status(ctx, eval, EvaluationStatus::Aborted).await?;
            return Ok(());
        }

        update_evaluation_status_with_error(
            ctx,
            eval,
            EvaluationStatus::Failed,
            error.to_owned(),
            Some("worker".to_string()),
        )
        .await?;
    }

    Ok(())
}

/// The shared blob's existence is acting as the circuit breaker.
/// The first corrupt failure is purging the blob and re-queuing the evaluation.
/// A recurring corruption is finding no blob and failing the evaluation instead of looping.
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

    requeue_evaluation(ctx, evaluation_id).await?;
    info!(%evaluation_id, %fingerprint, "purged corrupt eval-cache blob; re-queued eval for fresh evaluation");
    Ok(true)
}

async fn requeue_evaluation(ctx: &DbContext, evaluation_id: EvaluationId) -> Result<()> {
    if let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&ctx.worker_db)
        .await?
        && !matches!(
            eval.status,
            EvaluationStatus::Completed | EvaluationStatus::Failed | EvaluationStatus::Aborted
        )
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
            gradient_db::status::record_evaluation_message(
                ctx,
                evaluation.id,
                MessageLevel::Warning,
                missing_artefact_message(product),
                Some("builder".to_owned()),
            )
            .await;
        }
    }
    Ok(())
}

fn missing_artefact_message(product: &BuildProduct) -> String {
    format!(
        "artefact {} ({} {}) is declared in hydra-build-products but missing from the build output",
        product.path, product.file_type, product.subtype
    )
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

async fn requeue_imports(ctx: &DbContext, derivations: &[DerivationId]) -> Result<()> {
    let changes =
        gradient_db::graph::promotion::requeue_failed_import_closure(&ctx.worker_db, derivations)
            .await
            .context("requeue the failed closure of an import")?;
    emit_transition_effects(ctx, &changes).await?;

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

    if let Some(metrics) = policy::history_sample(metrics, policy::BuildEnd::Failed) {
        record_metrics(ctx, &shared_build, shared_build.derivation, &metrics).await;
    }

    if let Some(attempt_id) =
        gradient_db::scheduling::build_attempt::latest_attempt_id(&ctx.worker_db, shared_build.id)
            .await
            .ok()
            .flatten()
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
    let max_attempts = ctx.config.build.max_attempts;

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

    let max_loops = ctx.config.build.inputs_unavailable_max_loops;
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
        threshold: i64::from(ctx.config.build.substitute_miss_escalation_threshold),
    };
    let outcome = match policy::decide_failure_outcome(kind, attempt, max_attempts, substitution) {
        FailureOutcome::Retry if inputs_circuit_open => FailureOutcome::Permanent,
        other => other,
    };

    if let Err(e) = fail_latest_attempt(
        &ctx.worker_db,
        derivation_build,
        policy::attempt_outcome(kind),
        policy::attempt_reason_for(kind, outcome),
        Some(policy::truncate_failure_message(error)),
    )
    .await
    {
        warn!(%derivation_build, error = %e, "failed to record attempt failure reason");
    }

    match outcome {
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

gradient_db::sql! {
    CLEAR_SHARED_BUILD_SUBSTITUTION = "UPDATE derivation_build SET cache_available = false, status = $2, attempt = 0, \
         updated_at = (now() AT TIME ZONE 'UTC') WHERE id = $1",
        params = [SharedBuildId, Int(0)];

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
    db.execute_raw(CLEAR_SHARED_BUILD_SUBSTITUTION.bind([
        shared_build.id.into_inner().into(),
        i32::from(BuildStatus::Created).into(),
    ]))
    .await
    .context("clear the shared build's substitution")?;
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
    changes.extend(gradient_db::graph::can_start::promote(db, &[shared_build.derivation]).await?);
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
        for job in jobs.values().flatten() {
            gradient_db::status::insert_evaluation_message(
                db,
                job.evaluation,
                MessageLevel::Warning,
                format!(
                    "substitution of {} exhausted after {misses} misses; it will be built",
                    drv.store_path()
                ),
                Some("scheduler".to_owned()),
            )
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

async fn assigned(
    ctx: &DbContext,
    evaluation: EvaluationId,
    derivation_build: DerivationBuildId,
    dispatched_job: DispatchedJobId,
    substitute: bool,
    build_context: serde_json::Value,
) {
    if let Some(build_job) = find_or_create_build_job(ctx, evaluation, derivation_build).await {
        let superseded = gradient_db::scheduling::build_attempt::latest_attempt_id(
            &ctx.worker_db,
            derivation_build,
        )
        .await;
        match gradient_db::scheduling::build_attempt::open_attempt(
            &ctx.worker_db,
            build_job,
            derivation_build,
            dispatched_job,
            substitute,
            build_context,
        )
        .await
        {
            Ok(_) => finalize_superseded_log(ctx, superseded).await,
            Err(e) => warn!(error = %e, "failed to open build_attempt"),
        }
    }

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
    derivation_build: DerivationBuildId,
) -> Option<BuildJobId> {
    let shared_build = match EDerivationBuild::find_by_id(derivation_build)
        .one(&ctx.worker_db)
        .await
    {
        Ok(Some(a)) => a,
        Ok(None) => {
            warn!(%derivation_build, "shared build missing while opening build_attempt");
            return None;
        }
        Err(e) => {
            warn!(error = %e, %derivation_build, "shared build lookup failed while opening build_attempt");
            return None;
        }
    };

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
        derivation_build,
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
    use super::*;
    use crate::test_ctx::ctx;
    use gradient_entity::build_attempt::AttemptOutcome;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    #[tokio::test]
    async fn a_canceled_eval_job_puts_its_evaluation_back_in_the_queue() {
        let evaluating = MEvaluation {
            id: EvaluationId::now_v7(),
            status: EvaluationStatus::EvaluatingFlake,
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![evaluating.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let log_db = db.clone();
        let (ctx, _) = ctx(db).await;

        let _ = apply(
            &ctx,
            Transition::EvalFailed {
                evaluation: evaluating.id,
                error: "the worker stopped before the job finished".into(),
                kind: BuildFailureKind::Canceled,
                missing_paths: Vec::new(),
            },
        )
        .await;

        let log = log_db.into_transaction_log();
        let updates: Vec<_> = log
            .iter()
            .flat_map(|t| t.statements())
            .filter(|s| s.sql.starts_with("UPDATE \"evaluation\""))
            .collect();
        assert_eq!(updates.len(), 1, "{updates:?}");
        assert_eq!(
            updates[0].values.as_ref().map(|v| v.0[0].clone()),
            Some(sea_orm::Value::Int(Some(i32::from(
                EvaluationStatus::Queued
            )))),
            "{updates:?}"
        );
    }

    #[tokio::test]
    async fn an_orphaned_build_closes_its_running_attempt_before_it_is_queued_again() {
        let building = MDerivationBuild {
            id: DerivationBuildId::now_v7(),
            derivation: DerivationId::now_v7(),
            status: BuildStatus::Building,
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![building.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let log_db = db.clone();
        let (ctx, _) = ctx(db).await;

        let _ = apply(
            &ctx,
            Transition::OrphanedBuilds {
                shared_builds: vec![building.id],
            },
        )
        .await;

        let log = log_db.into_transaction_log();
        let statements: Vec<_> = log.iter().flat_map(|t| t.statements()).collect();
        let position = |prefix: &str| {
            statements
                .iter()
                .position(|s| s.sql.starts_with(prefix))
                .unwrap_or_else(|| panic!("no {prefix}: {statements:?}"))
        };
        let close = position("UPDATE \"build_attempt\" SET \"outcome\" = $1");
        let requeue = position("UPDATE \"derivation_build\"");
        assert!(close < requeue, "{statements:?}");

        let close = statements[close];
        assert!(
            close.sql.contains("\"build_attempt\".\"outcome\" = $"),
            "only the running attempt is closed: {}",
            close.sql
        );
        let values = format!("{:?}", close.values);
        assert!(values.contains(&building.id.to_string()), "{values}");
        assert!(
            values.contains(&format!(
                "Int(Some({}))",
                i32::from(AttemptOutcome::Aborted)
            )),
            "{values}"
        );
    }

    #[tokio::test]
    async fn an_import_requeue_thaws_the_failed_closure_of_the_imported_derivations() {
        let imported = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([
                Vec::<std::collections::BTreeMap<String, sea_orm::Value>>::new(),
            ])
            .into_connection();
        let log_db = db.clone();
        let (ctx, _) = ctx(db).await;

        apply(
            &ctx,
            Transition::RequeueImports {
                derivations: vec![imported],
            },
        )
        .await
        .unwrap();

        let log = log_db.into_transaction_log();
        let requeues: Vec<_> = log
            .iter()
            .flat_map(|t| t.statements())
            .filter(|s| s.sql.contains("UPDATE derivation_build db"))
            .collect();
        assert_eq!(requeues.len(), 1, "{log:?}");
        assert!(
            requeues[0].sql.contains("deterministic_blocked"),
            "a reproducible builder failure stays failed: {}",
            requeues[0].sql
        );
        assert!(
            format!("{:?}", requeues[0].values).contains(&imported.to_string()),
            "{:?}",
            requeues[0].values
        );
    }
}
