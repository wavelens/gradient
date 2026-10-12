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
pub struct OnlineWorkerLoad {
    pub online: usize,
    pub busy_pct: u8,
}

impl OnlineWorkerLoad {
    pub fn from_slots(slots: &[(i64, i64)]) -> Self {
        let (assigned, capacity) = slots.iter().fold((0, 0), |(a, c), (x, y)| (a + x, c + y));
        let busy_pct = if capacity > 0 {
            (assigned * 100 / capacity).clamp(0, 100) as u8
        } else {
            0
        };
        OnlineWorkerLoad {
            online: slots.len(),
            busy_pct,
        }
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct DashboardStats {
    pub cpu_time_ms: i64,
    pub cpu_time_ms_7d: i64,
    pub builds_completed: i64,
    pub cache_size_bytes: i64,
    pub workers: OnlineWorkerLoad,
    pub queue_wait_p50_ms: i64,
}

async fn visible_slots(scheduler: &Scheduler, scope: &MetricsScope) -> Vec<(i64, i64)> {
    scheduler
        .board_workers()
        .await
        .into_iter()
        .filter(|w| scope.worker_projects(w.authorized_peers.as_ref()).is_some())
        .map(|w| {
            (
                w.assigned_build_count as i64,
                w.max_concurrent_builds as i64,
            )
        })
        .collect()
}

#[utoipa::path(
    get,
    path = "/dashboard/stats",
    operation_id = "getDashboardStats",
    tag = "dashboard",
    summary = "Stat tiles of the current user",
    description = "Totals over the projects the viewer may see (all for a superuser, public plus member projects otherwise).\n`builds_completed` counts recorded derivation metrics; `queue_wait_p50_ms` is the median wait for a worker over the last 24 hours.\n`cache_size_bytes` sums the stored size of each cache the viewer created or reaches through a project membership (all caches for a superuser); each size is recounted every metrics rollup pass, and a path held by two caches counts in both.\n`workers` covers connected workers of visible projects only: `busy_pct` is assigned jobs over summed capacity, capped at 100.",
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
        workers: OnlineWorkerLoad::from_slots(&visible_slots(&scheduler, &scope).await),
        queue_wait_p50_ms: totals.queue_wait_p50_ms,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_is_assigned_over_capacity() {
        assert_eq!(
            OnlineWorkerLoad::from_slots(&[(3, 4), (1, 4)]),
            OnlineWorkerLoad {
                online: 2,
                busy_pct: 50
            }
        );
    }

    #[test]
    fn no_workers_is_idle_not_a_division_by_zero() {
        assert_eq!(
            OnlineWorkerLoad::from_slots(&[]),
            OnlineWorkerLoad {
                online: 0,
                busy_pct: 0
            }
        );
        assert_eq!(
            OnlineWorkerLoad::from_slots(&[(0, 0)]),
            OnlineWorkerLoad {
                online: 1,
                busy_pct: 0
            }
        );
    }

    #[test]
    fn overbooked_caps_at_full() {
        assert_eq!(OnlineWorkerLoad::from_slots(&[(9, 4)]).busy_pct, 100);
    }
}
