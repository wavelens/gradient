/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! One handler per [`Transition`], each run inside the actor's transaction.

use std::collections::HashMap;

use anyhow::Result;
use gradient_db::{DbContext, update_evaluation_status, update_evaluation_status_with_error};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::proto::BuildFailureKind;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tracing::{error, info, warn};

use crate::ingest::{EvalEdgeAccumulator, flush_deferred_deps};
use crate::messages::{Transition, TransitionReport};

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
            Ok(TransitionReport { aborted_anchors })
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
