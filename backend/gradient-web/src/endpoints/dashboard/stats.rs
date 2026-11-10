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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkerLoad {
    pub assigned_jobs: usize,
    pub max_concurrent_builds: usize,
}

impl WorkerLoad {
    fn busy_fraction(self) -> f64 {
        if self.max_concurrent_builds == 0 {
            return f64::from(u8::from(self.assigned_jobs > 0));
        }
        (self.assigned_jobs as f64 / self.max_concurrent_builds as f64).clamp(0.0, 1.0)
    }
}

impl OnlineWorkerLoad {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "average of fractions clamped to 0..=1 can't exceed 100"
    )]
    pub fn from_workers(workers: &[WorkerLoad]) -> Self {
        let busy_pct = if workers.is_empty() {
            0
        } else {
            let busy: f64 = workers.iter().map(|w| w.busy_fraction()).sum();
            (busy * 100.0 / workers.len() as f64).round() as u8
        };
        OnlineWorkerLoad {
            online: workers.len(),
            busy_pct,
        }
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct DashboardStats {
    pub cpu_time_ms: Option<i64>,
    pub cpu_time_ms_7d: Option<i64>,
    pub builds_completed: i64,
    pub cache_size_bytes: i64,
    pub workers: OnlineWorkerLoad,
    pub queue_wait_p50_ms: i64,
}

async fn visible_loads(scheduler: &Scheduler, scope: &MetricsScope) -> Vec<WorkerLoad> {
    scheduler
        .board_workers()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|w| scope.worker_projects(w.authorized_peers.as_ref()).is_some())
        .map(|w| WorkerLoad {
            assigned_jobs: w.assigned_job_count,
            max_concurrent_builds: w.max_concurrent_builds as usize,
        })
        .collect()
}

#[utoipa::path(
    get,
    path = "/dashboard/stats",
    operation_id = "getDashboardStats",
    tag = "dashboard",
    summary = "Stat tiles of the current user",
    description = "Totals over the projects the viewer may see (all for a superuser, public plus member projects otherwise).\n`builds_completed` counts the builds that ran to completion in those projects (substituted builds excluded, a build reused by several evaluations counts once); `queue_wait_p50_ms` is the median wait for a worker over the last 24 hours.\n`cpu_time_ms` and `cpu_time_ms_7d` are null if no workers report CPU time (build metrics are off on the worker).\n`cache_size_bytes` sums the stored size of each cache the viewer created or reaches through a project membership (all caches for a superuser); each size is recounted every metrics rollup pass, and a path held by two caches counts in both.\nOnly connected workers of visible projects appear in ``workers``. ``busy_pct`` is the average slot use across those workers: assigned jobs relative to ``max_concurrent_builds``, with 100 as the maximum.",
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
        workers: OnlineWorkerLoad::from_workers(&visible_loads(&scheduler, &scope).await),
        queue_wait_p50_ms: totals.queue_wait_p50_ms,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jobs(assigned_jobs: usize, max_concurrent_builds: usize) -> WorkerLoad {
        WorkerLoad {
            assigned_jobs,
            max_concurrent_builds,
        }
    }

    fn busy(workers: &[WorkerLoad]) -> u8 {
        OnlineWorkerLoad::from_workers(workers).busy_pct
    }

    #[test]
    fn busy_is_the_mean_of_each_worker_load() {
        assert_eq!(
            OnlineWorkerLoad::from_workers(&[jobs(3, 4), jobs(1, 4)]),
            OnlineWorkerLoad {
                online: 2,
                busy_pct: 50
            }
        );
    }

    #[test]
    fn one_job_on_a_wide_worker_does_not_round_to_zero() {
        assert_eq!(busy(&[jobs(1, 64)]), 2);
        assert_eq!(busy(&[jobs(1, 0)]), 100);
    }

    #[test]
    fn no_workers_is_idle_not_a_division_by_zero() {
        assert_eq!(
            OnlineWorkerLoad::from_workers(&[]),
            OnlineWorkerLoad {
                online: 0,
                busy_pct: 0
            }
        );
        assert_eq!(busy(&[jobs(0, 0)]), 0);
    }

    #[test]
    fn overbooked_caps_at_full() {
        assert_eq!(busy(&[jobs(9, 4)]), 100);
    }
}
