/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::WebResult;
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct WorkerLoad {
    pub online: usize,
    pub busy_pct: u8,
}

impl WorkerLoad {
    pub fn from_slots(slots: &[(i64, i64)]) -> Self {
        let (assigned, capacity) = slots.iter().fold((0, 0), |(a, c), (x, y)| (a + x, c + y));
        let busy_pct = if capacity > 0 {
            (assigned * 100 / capacity).clamp(0, 100) as u8
        } else {
            0
        };
        WorkerLoad {
            online: slots.len(),
            busy_pct,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DashboardStats {
    pub cpu_time_ms: i64,
    pub cpu_time_ms_7d: i64,
    pub builds_completed: i64,
    pub cache_size_bytes: i64,
    pub workers: WorkerLoad,
    pub queue_wait_p50_ms: i64,
}

async fn visible_slots(scheduler: &Scheduler, scope: &MetricsScope) -> Vec<(i64, i64)> {
    scheduler
        .board_workers()
        .await
        .into_iter()
        .filter(|w| scope.worker_projects(w.authorized_peers.as_ref()).is_some())
        .map(|w| (w.assigned_job_count as i64, w.max_concurrent_builds as i64))
        .collect()
}

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
        workers: WorkerLoad::from_slots(&visible_slots(&scheduler, &scope).await),
        queue_wait_p50_ms: totals.queue_wait_p50_ms,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_is_assigned_over_capacity() {
        assert_eq!(
            WorkerLoad::from_slots(&[(3, 4), (1, 4)]),
            WorkerLoad {
                online: 2,
                busy_pct: 50
            }
        );
    }

    #[test]
    fn no_workers_is_idle_not_a_division_by_zero() {
        assert_eq!(
            WorkerLoad::from_slots(&[]),
            WorkerLoad {
                online: 0,
                busy_pct: 0
            }
        );
        assert_eq!(
            WorkerLoad::from_slots(&[(0, 0)]),
            WorkerLoad {
                online: 1,
                busy_pct: 0
            }
        );
    }

    #[test]
    fn overbooked_caps_at_full() {
        assert_eq!(WorkerLoad::from_slots(&[(9, 4)]).busy_pct, 100);
    }
}
