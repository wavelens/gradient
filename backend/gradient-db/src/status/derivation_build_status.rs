/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! State-machine-guarded transitions of the global `derivation_build` anchor.
//! One anchor transition fans out to every evaluation that references the
//! derivation (its `build_job`s): board events, per-eval CI reactor calls, and
//! a single global entry-point dep-count delta. Graph-driven promotion runs on
//! terminal-success; dependency-failure cascades on terminal-failure.

use super::effects::{TransitionChange, emit_transition_effects};
use super::logging::{PHASE_SUBJECT_BUILD, finalize_build_log, record_phase_event};
use crate::DbContext;
use crate::state_machine::BuildStateMachine;
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use std::collections::HashMap;
use tracing::{error, info};

pub async fn update_derivation_build_status(
    ctx: &DbContext,
    anchor: MDerivationBuild,
    status: BuildStatus,
) -> MDerivationBuild {
    if anchor.status == status {
        return anchor;
    }

    if let Err(e) = BuildStateMachine::validate(anchor.status, status) {
        error!(
            derivation_build = %anchor.id,
            from = ?anchor.status,
            to = ?status,
            error = %e,
            "Skipping invalid anchor status transition - status update lost or out of order"
        );
        return anchor;
    }

    info!(derivation_build = %anchor.id, derivation = %anchor.derivation, from = ?anchor.status, to = ?status, "anchor status transition");

    let now = gradient_types::now();
    let prev_status = anchor.status;
    let mut active: ADerivationBuild = anchor.clone().into_active_model();
    active.status = Set(status);
    active.updated_at = Set(now);
    if status == BuildStatus::Queued && anchor.queued_at.is_none() {
        active.queued_at = Set(Some(now));
    }

    if status == BuildStatus::Building {
        let _ = crate::build_attempt::stamp_attempt_started(&ctx.worker_db, anchor.id, now).await;
    }

    if BuildStateMachine::is_terminal(&status) {
        let _ = crate::build_attempt::stamp_attempt_finished(&ctx.worker_db, anchor.id, now).await;
    }

    let updated = match active.update(&ctx.worker_db).await {
        Ok(u) => u,
        Err(e) => {
            error!(error = %e, derivation_build = %anchor.id, "Failed to update anchor status");
            return anchor;
        }
    };

    // All fan-out (dep-count delta, board events, CI reactor, cache-changed)
    // goes through the one effects emitter - the same path the bulk sweeps
    // feed - so the reactive and proactive models can never drift apart.
    emit_transition_effects(
        ctx,
        &[TransitionChange { derivation: updated.derivation, from: prev_status, to: status }],
    )
    .await;

    if matches!(status, BuildStatus::Completed | BuildStatus::Substituted) {
        // Recompute closure-completeness up the build-dependency graph from this
        // anchor, before promoting. A built anchor becomes `closure_complete` once
        // its build deps are each `closure_complete` or `substitutable`; this also
        // ripples to dependents that were waiting only on this one. Doing it before
        // `promote_dependents` is essential - otherwise the last dep to land
        // strands its dependents behind a flag that flips only afterward.
        if let Err(e) = crate::promotion::propagate_closure_complete(&ctx.worker_db, updated.derivation).await {
            error!(error = %e, "failed to propagate closure_complete");
        }

        match crate::promotion::promote_dependents(&ctx.worker_db, updated.derivation).await {
            Ok(changes) => emit_transition_effects(ctx, &changes).await,
            Err(e) => error!(error = %e, "failed to promote dependents"),
        }
    }

    if matches!(
        status,
        BuildStatus::FailedPermanent | BuildStatus::FailedTimeout | BuildStatus::DependencyFailed
    ) {
        match crate::promotion::cascade_dependency_failed(&ctx.worker_db, updated.derivation).await {
            Ok(changes) => emit_transition_effects(ctx, &changes).await,
            Err(e) => error!(error = %e, "failed to cascade dependency failure"),
        }
    }

    let pe_ctx = ctx.clone();
    let pe_worker = crate::build_attempt::latest_attempt_worker(&ctx.worker_db, updated.id)
        .await
        .ok()
        .flatten();
    let pe_id = updated.id.into_inner();
    ctx.shutdown.spawn(async move {
        record_phase_event(
            &pe_ctx.worker_db,
            PHASE_SUBJECT_BUILD,
            pe_id,
            i32::from(status) as i16,
            pe_worker,
            now,
        )
        .await;
    });

    if BuildStateMachine::is_terminal(&status)
        && let Ok(Some(attempt_id)) =
            crate::build_attempt::latest_attempt_id(&ctx.worker_db, updated.id).await
    {
        let log_ctx = ctx.clone();
        ctx.shutdown.spawn(async move {
            finalize_build_log(&log_ctx, attempt_id).await;
        });
    }

    updated
}

/// Re-announce the current status of `derivations` through the effects emitter
/// (board events + per-entry-point forge checks). For callers that only know
/// the affected derivation set, not the transitions that produced it - e.g.
/// state import; paths with the actual changes in hand should call
/// [`emit_transition_effects`] directly.
pub async fn notify_build_status_for_derivations(ctx: &DbContext, derivations: &[DerivationId]) {
    if derivations.is_empty() {
        return;
    }

    let db = &ctx.worker_db;
    let status_by_drv: HashMap<DerivationId, BuildStatus> =
        crate::fetch_in_chunks(derivations, |chunk| async move {
            EDerivationBuild::find()
                .filter(CDerivationBuild::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|a| (a.derivation, a.status))
        .collect();

    let changes: Vec<TransitionChange> = status_by_drv
        .into_iter()
        .map(|(derivation, status)| TransitionChange::unchanged(derivation, status))
        .collect();
    emit_transition_effects(ctx, &changes).await;
}
