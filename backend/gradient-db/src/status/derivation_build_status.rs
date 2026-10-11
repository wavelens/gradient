/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::effects::{TransitionChange, emit_transition_effects};
use super::logging::{PhaseSubjectKind, record_phase_event};
use crate::DbContext;
use gradient_entity::build::BuildStatus;
use gradient_entity::transition::Transition;
use gradient_types::*;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter, UpdateMany};
use std::collections::{HashMap, HashSet};
use tracing::{error, info};

pub fn build_change(transition: Transition<BuildStatus>) -> UpdateMany<EDerivationBuild> {
    EDerivationBuild::update_many()
        .col_expr(CDerivationBuild::Status, Expr::value(transition.to))
        .filter(CDerivationBuild::Status.is_in(transition.from.iter().copied()))
}

pub async fn update_derivation_build_status(
    ctx: &DbContext,
    shared_build: MDerivationBuild,
    status: BuildStatus,
) -> Result<MDerivationBuild, DbErr> {
    if shared_build.status == status {
        return Ok(shared_build);
    }

    if !status.sources().contains(&shared_build.status) {
        error!(
            derivation_build = %shared_build.id,
            from = ?shared_build.status,
            to = ?status,
            "Skipping invalid shared build status transition - status update lost or out of order"
        );
        return Ok(shared_build);
    }

    info!(derivation_build = %shared_build.id, derivation = %shared_build.derivation, from = ?shared_build.status, to = ?status, "shared build status transition");

    let now = gradient_types::now();
    let prev_status = shared_build.status;
    let mut change =
        build_change(status.report()).col_expr(CDerivationBuild::UpdatedAt, Expr::value(now));
    if status == BuildStatus::Queued && shared_build.queued_at.is_none() {
        change = change.col_expr(CDerivationBuild::QueuedAt, Expr::value(Some(now)));
    }

    if status == BuildStatus::Building {
        crate::scheduling::build_attempt::stamp_attempt_started(
            &ctx.worker_db,
            shared_build.id,
            now,
        )
        .await?;
    }

    if status.is_terminal() {
        crate::scheduling::build_attempt::stamp_attempt_finished(
            &ctx.worker_db,
            shared_build.id,
            now,
        )
        .await?;
    }

    let changed = change
        .filter(CDerivationBuild::Id.eq(shared_build.id))
        .exec_with_returning(&ctx.worker_db)
        .await?;
    let Some(updated) = changed.into_iter().next() else {
        return Ok(EDerivationBuild::find_by_id(shared_build.id)
            .one(&ctx.worker_db)
            .await?
            .unwrap_or(shared_build));
    };

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
        i16::from(status),
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
                status: i16::from(status),
                ..Default::default()
            },
        )
        .await?;
    }
    ctx.delivery_wake.notify_one();

    Ok(())
}
