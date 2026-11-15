/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::metrics_scope::MetricsScope;
use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::dashboard::{cache_size, scoped_totals};
use gradient_scheduler::Scheduler;
use gradient_types::*;
use serde::Serialize;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct VisibleWorkerBuildSlotCounts {
    pub used_build_slot_count: usize,
    pub total_build_slots: usize,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct DashboardStats {
    pub cpu_time_ms: Option<i64>,
    pub cpu_time_ms_7d: Option<i64>,
    pub builds_completed: i64,
    pub cache_size_bytes: i64,
    pub workers: VisibleWorkerBuildSlotCounts,
    pub queue_wait_p50_ms: i64,
}

async fn sum_visible_connected_worker_build_slots(
    scheduler: &Scheduler,
    scope: &MetricsScope,
) -> VisibleWorkerBuildSlotCounts {
    scheduler
        .board_workers()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|w| scope.worker_projects(w.authorized_peers.as_ref()).is_some())
        .fold(VisibleWorkerBuildSlotCounts::default(), |sum, w| {
            VisibleWorkerBuildSlotCounts {
                used_build_slot_count: sum.used_build_slot_count + w.assigned_build_count,
                total_build_slots: sum.total_build_slots + w.max_concurrent_builds as usize,
            }
        })
}

#[utoipa::path(
    get,
    path = "/dashboard/stats",
    operation_id = "getDashboardStats",
    tag = "dashboard",
    summary = "Stat tiles of the current user",
    description = "Totals over the projects the viewer may see (all for a superuser, public plus member projects otherwise).\n`builds_completed` counts the builds that ran to completion in those projects (substituted builds excluded, a build reused by several evaluations counts once); `queue_wait_p50_ms` is the median wait for a worker over the last 24 hours.\n`cpu_time_ms` and `cpu_time_ms_7d` are null if no workers report CPU time (build metrics are off on the worker).\n`cache_size_bytes` sums the stored size of each cache the viewer created or reaches through a project membership (all caches for a superuser); each size is recounted every metrics rollup pass, and a path held by two caches counts in both.\n`workers` cover connected workers in viewable projects. `used_build_slot_count` is the count of build jobs assigned to them. `total_build_slots` is the sum of their `max_concurrent_builds`.",
    responses((status = OK, description = "Dashboard stat tiles", body = DashboardStats), WebError)
)]
pub async fn get_stats(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<DashboardStats>>> {
    let scope = MetricsScope::resolve(&state.web_db, &Some(user.clone())).await?;
    let totals = scoped_totals(&state.web_db, scope.project_in_list().as_deref()).await?;
    Ok(ok_json(DashboardStats {
        cpu_time_ms: totals.cpu_time_ms,
        cpu_time_ms_7d: totals.cpu_time_ms_7d,
        builds_completed: totals.builds_completed,
        cache_size_bytes: cache_size(&state.web_db, user.id, user.superuser).await?,
        workers: sum_visible_connected_worker_build_slots(&scheduler, &scope).await,
        queue_wait_p50_ms: totals.queue_wait_p50_ms,
    }))
}
