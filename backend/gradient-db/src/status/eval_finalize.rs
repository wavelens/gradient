/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Graph-derived evaluation finalization. An evaluation settles the moment its
//! last referenced anchor leaves the active set, regardless of WHICH mutation
//! path moved it - the effects emitter calls in here on every terminal
//! transition, so bulk sweeps and the single-row path finalize identically.

use super::evaluation_status::update_evaluation_status;
use crate::DbContext;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter};
use std::collections::HashSet;
use tracing::{info, warn};

/// Settle `evaluation_id` if the build graph says it is done: no referenced
/// anchor is still active (`Created`/`Queued`/`Building`/`FailedTransient`).
/// `Failed` when any anchor terminally failed or the eval logged error-level
/// messages (nix eval errors mean a partially-successful walk), else
/// `Completed`. A no-op unless the evaluation is currently `Building`.
pub async fn check_evaluation_done(
    ctx: &DbContext,
    evaluation_id: EvaluationId,
) -> Result<(), DbErr> {
    let statuses = crate::reachability::eval_anchor_statuses(&ctx.worker_db, evaluation_id).await?;

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
        .one(&ctx.worker_db)
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

    let eval_error_messages = EEvaluationMessage::find()
        .filter(CEvaluationMessage::Evaluation.eq(evaluation_id))
        .filter(
            CEvaluationMessage::Level.eq(gradient_entity::evaluation_message::MessageLevel::Error),
        )
        .all(&ctx.worker_db)
        .await?;

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
    // settled: a terminal eval has a fixed graph, so one recompute makes the
    // displayed bar exact even if any incremental delta was missed.
    if let Err(e) = crate::dep_closure::reconcile_eval_dep_counts(&ctx.worker_db, evaluation_id).await
    {
        warn!(error = %e, %evaluation_id, "reconcile_eval_dep_counts at eval settle failed");
    }

    update_evaluation_status(ctx, eval, target).await;
    Ok(())
}

/// Finalize every evaluation referencing any of `derivations`, deduplicated.
pub async fn finalize_evals_for_derivations(
    ctx: &DbContext,
    derivations: &[DerivationId],
) -> Result<(), DbErr> {
    let mut seen = HashSet::new();
    for &derivation in derivations {
        for evaluation_id in
            crate::reachability::evals_referencing_derivation(&ctx.worker_db, derivation).await?
        {
            if seen.insert(evaluation_id) {
                check_evaluation_done(ctx, evaluation_id).await?;
            }
        }
    }

    Ok(())
}
