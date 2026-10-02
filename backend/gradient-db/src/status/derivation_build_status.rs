/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::effects::{TransitionChange, emit_transition_effects};
use super::logging::{PhaseSubjectKind, record_phase_event};
use crate::DbContext;
use crate::state_machine::BuildStateMachine;
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, DbErr, EntityTrait, IntoActiveModel, QueryFilter};
use std::collections::{HashMap, HashSet};
use tracing::{error, info};

pub async fn update_derivation_build_status(
    ctx: &DbContext,
    shared_build: MDerivationBuild,
    status: BuildStatus,
) -> Result<MDerivationBuild, DbErr> {
    if shared_build.status == status {
        return Ok(shared_build);
    }

    if let Err(e) = BuildStateMachine::validate(shared_build.status, status) {
        error!(
            derivation_build = %shared_build.id,
            from = ?shared_build.status,
            to = ?status,
            error = %e,
            "Skipping invalid shared build status transition - status update lost or out of order"
        );
        return Ok(shared_build);
    }

    info!(derivation_build = %shared_build.id, derivation = %shared_build.derivation, from = ?shared_build.status, to = ?status, "shared build status transition");

    let now = gradient_types::now();
    let prev_status = shared_build.status;
    let mut active: ADerivationBuild = shared_build.clone().into_active_model();
    active.status = Set(status);
    active.updated_at = Set(now);
    if status == BuildStatus::Queued && shared_build.queued_at.is_none() {
        active.queued_at = Set(Some(now));
    }

    if status == BuildStatus::Building {
        crate::scheduling::build_attempt::stamp_attempt_started(
            &ctx.worker_db,
            shared_build.id,
            now,
        )
        .await?;
    }

    if BuildStateMachine::is_terminal(&status) {
        crate::scheduling::build_attempt::stamp_attempt_finished(
            &ctx.worker_db,
            shared_build.id,
            now,
        )
        .await?;
    }

    let updated = active.update(&ctx.worker_db).await?;

    emit_transition_effects(
        ctx,
        &[TransitionChange {
            derivation: updated.derivation,
            from: prev_status,
            to: status,
        }],
    )
    .await?;

    if matches!(status, BuildStatus::Completed | BuildStatus::Substituted) {
        let changes =
            crate::graph::can_start::advance_fetchable(&ctx.worker_db, &[updated.derivation])
                .await?;
        emit_transition_effects(ctx, &changes).await?;
    }

    if matches!(
        status,
        BuildStatus::FailedPermanent | BuildStatus::FailedTimeout | BuildStatus::DependencyFailed
    ) {
        let changes =
            crate::graph::promotion::cascade_dependency_failed(&ctx.worker_db, updated.derivation)
                .await?;
        emit_transition_effects(ctx, &changes).await?;
    }

    // The log write is awaited, not spawned.
    // A detached writer racing the emitter's pending deliveries once lost a phase timeline.
    let worker =
        crate::scheduling::build_attempt::latest_attempt_worker(&ctx.worker_db, updated.id).await?;
    record_phase_event(
        &ctx.worker_db,
        PhaseSubjectKind::Build,
        updated.id.into_inner(),
        i32::from(status) as i16,
        worker,
        now,
    )
    .await?;

    Ok(updated)
}

pub async fn notify_build_status_for_derivations(
    ctx: &DbContext,
    derivations: &[DerivationId],
) -> Result<(), DbErr> {
    if derivations.is_empty() {
        return Ok(());
    }

    let db = &ctx.worker_db;
    let status_by_drv: HashMap<DerivationId, BuildStatus> =
        crate::fetch_in_chunks(derivations, |chunk| async move {
            EDerivationBuild::find()
                .filter(CDerivationBuild::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await?
        .into_iter()
        .map(|a| (a.derivation, a.status))
        .collect();

    let changes: Vec<TransitionChange> = status_by_drv
        .into_iter()
        .map(|(derivation, status)| TransitionChange::unchanged(derivation, status))
        .collect();
    emit_transition_effects(ctx, &changes).await
}

pub async fn announce_entry_point_statuses(
    ctx: &DbContext,
    evaluation: EvaluationId,
    derivations: &[DerivationId],
) -> Result<(), DbErr> {
    if derivations.is_empty() {
        return Ok(());
    }

    let db = &ctx.worker_db;

    let entry_point_drvs: HashSet<DerivationId> =
        crate::fetch_in_chunks(derivations, |chunk| async move {
            EEntryPoint::find()
                .filter(CEntryPoint::Evaluation.eq(evaluation))
                .filter(CEntryPoint::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await?
        .into_iter()
        .map(|ep| ep.derivation)
        .collect();
    if entry_point_drvs.is_empty() {
        return Ok(());
    }

    let drv_ids: Vec<DerivationId> = entry_point_drvs.iter().copied().collect();
    let status_by_drv: HashMap<DerivationId, BuildStatus> =
        crate::fetch_in_chunks(&drv_ids, |chunk| async move {
            EDerivationBuild::find()
                .filter(CDerivationBuild::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await?
        .into_iter()
        .map(|a| (a.derivation, a.status))
        .collect();

    let jobs = crate::fetch_in_chunks(&drv_ids, |chunk| async move {
        EBuildJob::find()
            .filter(CBuildJob::Evaluation.eq(evaluation))
            .filter(CBuildJob::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await?;

    for job in jobs {
        let Some(&status) = status_by_drv.get(&job.derivation) else {
            continue;
        };

        crate::deliveries::events::record(
            db,
            &ctx.events,
            gradient_types::events::build::Reported {
                build_id: job.id,
                derivation_build: job.derivation_build,
                evaluation_id: job.evaluation,
                derivation: job.derivation,
                status: i32::from(status) as i16,
                ..Default::default()
            },
        )
        .await?;
    }
    ctx.delivery_wake.notify_one();

    Ok(())
}
