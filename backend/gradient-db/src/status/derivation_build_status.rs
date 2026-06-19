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

use super::logging::{PHASE_SUBJECT_BUILD, finalize_build_log, record_phase_event};
use crate::DbContext;
use crate::reachability::build_jobs_for_derivation;
use crate::state_machine::BuildStateMachine;
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, IntoActiveModel};
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

    // Global entry-point dep-count delta: one statement shifts the unit across
    // every entry point (in any eval) whose closure contains this derivation.
    let dep_ctx = ctx.clone();
    let drv = updated.derivation;
    let (old_i, new_i) = (i32::from(prev_status), i32::from(status));
    ctx.shutdown.spawn(async move {
        if let Err(e) =
            crate::dep_closure::apply_dep_count_delta(&dep_ctx.worker_db, drv, old_i, new_i).await
        {
            error!(error = %e, "failed to update entry-point dep counts");
        }
    });

    // Fan side-effects across every eval that references the derivation.
    let jobs = build_jobs_for_derivation(&ctx.worker_db, updated.derivation)
        .await
        .unwrap_or_default();
    for job in &jobs {
        let _ = ctx
            .board_events
            .send(gradient_types::BoardEvent::BuildStatusChanged {
                evaluation_id: job.evaluation.into_inner(),
                build_id: job.id.into_inner(),
                status: i32::from(status) as i16,
            });
    }

    if matches!(status, BuildStatus::Completed | BuildStatus::Substituted) {
        let _ = ctx
            .board_events
            .send(gradient_types::BoardEvent::CacheChanged);
        if let Err(e) = crate::promotion::promote_dependents(&ctx.worker_db, updated.derivation).await
        {
            error!(error = %e, "failed to promote dependents");
        }
    }

    if matches!(
        status,
        BuildStatus::FailedPermanent | BuildStatus::FailedTimeout | BuildStatus::DependencyFailed
    ) && let Err(e) =
        crate::promotion::cascade_dependency_failed(&ctx.worker_db, updated.derivation).await
    {
        error!(error = %e, "failed to cascade dependency failure");
    }

    if BuildStateMachine::is_terminal(&status) {
        for job in jobs {
            let action_ctx = ctx.clone();
            ctx.shutdown.spawn(async move {
                action_ctx.reactor.on_build_terminal(&action_ctx, job, status).await;
            });
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
