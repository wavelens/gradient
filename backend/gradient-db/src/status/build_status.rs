/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::logging::{PHASE_SUBJECT_BUILD, finalize_build_log, record_phase_event};
use crate::DbContext;
use crate::state_machine::BuildStateMachine;
use gradient_types::*;
use gradient_entity::build::BuildStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, IntoActiveModel};
use tracing::{error, info};

pub async fn update_build_status(ctx: &DbContext, build: MBuild, status: BuildStatus) -> MBuild {
    if build.status == status {
        return build;
    }

    match BuildStateMachine::validate(build.status, status) {
        Ok(_) => {}
        Err(e) => {
            // Loud: a rejected transition usually means the build is stuck
            // in a state the next event can't legally move it from - e.g.
            // a JobFailed arriving while the build is still `Queued`
            // because the worker's `Building` JobUpdate was lost / never
            // sent. Without this we'd silently drop the failure and the UI
            // would show the build hanging in `Queued` / `Building` forever.
            error!(
                build_id = %build.id,
                from = ?build.status,
                to = ?status,
                error = %e,
                "Skipping invalid build status transition - investigate: status update lost or out of order"
            );
            return build;
        }
    }

    info!(build_id = %build.id, from = ?build.status, to = ?status, "build status transition");

    let mut active_build: ABuild = build.clone().into_active_model();

    let event_status = status;
    let now = gradient_types::now();
    // When transitioning out of `Building` into a terminal state, record the
    // elapsed wall-clock time. `build.updated_at` is the timestamp of the
    // previous transition (into `Building` by `Scheduler::handle_build_status_update`).
    if build.status == BuildStatus::Building
        && matches!(
            status,
            BuildStatus::Completed
                | BuildStatus::FailedPermanent
                | BuildStatus::FailedTimeout
                | BuildStatus::Aborted
                | BuildStatus::DependencyFailed
        )
        && build.build_time_ms.is_none()
    {
        let elapsed_ms = (now - build.updated_at).num_milliseconds().max(0);
        active_build.build_time_ms = Set(Some(elapsed_ms));
    }
    active_build.status = Set(status);
    active_build.updated_at = Set(now);
    if status == BuildStatus::Queued && build.queued_at.is_none() {
        active_build.queued_at = Set(Some(now));
    }
    if status == BuildStatus::Building {
        active_build.build_started_at = Set(Some(now));
    }
    if matches!(
        status,
        BuildStatus::Completed
            | BuildStatus::Substituted
            | BuildStatus::FailedPermanent
            | BuildStatus::FailedTimeout
            | BuildStatus::Aborted
            | BuildStatus::DependencyFailed
    ) {
        active_build.build_finished_at = Set(Some(now));
    }

    match active_build.update(&ctx.worker_db).await {
        Ok(updated_build) => {
            let _ = ctx
                .board_events
                .send(gradient_types::BoardEvent::BuildStatusChanged {
                    evaluation_id: updated_build.evaluation.into_inner(),
                    build_id: updated_build.id.into_inner(),
                    status: i32::from(event_status) as i16,
                });
            if matches!(
                updated_build.status,
                BuildStatus::Completed | BuildStatus::Substituted
            ) {
                let _ = ctx
                    .board_events
                    .send(gradient_types::BoardEvent::CacheChanged);
            }

            let action_ctx = ctx.clone();
            let action_build = updated_build.clone();
            ctx.shutdown.spawn(async move {
                action_ctx
                    .reactor
                    .on_build_terminal(&action_ctx, action_build, event_status)
                    .await;
            });

            let pe_ctx = ctx.clone();
            let pe_worker = updated_build.worker.clone();
            let pe_id = updated_build.id.into_inner();
            ctx.shutdown.spawn(async move {
                record_phase_event(
                    &pe_ctx.worker_db,
                    PHASE_SUBJECT_BUILD,
                    pe_id,
                    i32::from(event_status) as i16,
                    pe_worker,
                    now,
                )
                .await;
            });

            // On terminal state, compress the build log into zstd chunks and
            // record the chunk index, then drop the inline copy so the chunks
            // are the sole at-rest representation.
            if matches!(
                updated_build.status,
                BuildStatus::Completed
                    | BuildStatus::Substituted
                    | BuildStatus::FailedPermanent
                    | BuildStatus::FailedTimeout
                    | BuildStatus::Aborted
                    | BuildStatus::DependencyFailed
            ) {
                let log_ctx = ctx.clone();
                let log_id = updated_build.log_id.unwrap_or(updated_build.id);
                ctx.shutdown.spawn(async move {
                    finalize_build_log(&log_ctx, log_id).await;
                });
            }

            updated_build
        }
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to update build status");
            build
        }
    }
}
