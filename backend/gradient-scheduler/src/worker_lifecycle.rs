/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `Scheduler` methods for worker connect / disconnect / capability management.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
};
use tracing::{debug, info, warn};

use gradient_types::ids::ProjectId;
use gradient_types::proto::GradientCapabilities;

use crate::Scheduler;
use crate::actor::{
    Registered, Registration, SchedulerMsg, SessionPort, WorkerCapabilities, WorkerMetrics,
};
use crate::build;
use crate::jobs::PendingJob;

/// Insert a `worker_sample` time-series row for a connected worker. Best-effort;
/// skipped when the worker's owning project is unknown. Called from the heartbeat loop.
pub(crate) async fn record_worker_sample(
    db: &impl sea_orm::ConnectionTrait,
    info: &crate::WorkerInfo,
) {
    let Some(project) = info.project else {
        return;
    };
    let sample = gradient_entity::worker_sample::Model {
        id: gradient_entity::ids::WorkerSampleId::now_v7(),
        worker_id: info.id.clone(),
        project,
        at: gradient_types::now(),
        cpu_usage_pct: info.cpu_usage_pct,
        ram_free_mb: info.ram_free_mb.map(|v| v as i64),
        ram_total_mb: Some(info.ram_total_mb as i64),
        disk_speed_mbps: info.disk_speed_mbps,
        network_speed_mbps: info.network_speed_mbps,
        assigned_jobs: info.assigned_job_count as i32,
        max_concurrent_builds: info.max_concurrent_builds as i32,
        state: info.draining.into(),
        capabilities: serde_json::to_value(&info.capabilities).unwrap_or(serde_json::Value::Null),
    }
    .into_active_model();

    if let Err(e) = gradient_entity::worker_sample::Entity::insert(sample)
        .exec(db)
        .await
    {
        warn!(error = %e, worker_id = %info.id, "failed to insert worker_sample");
    }
}

impl Scheduler {
    pub async fn is_worker_connected(&self, worker_id: &str) -> bool {
        let worker = worker_id.to_owned();
        self.call(|reply| SchedulerMsg::IsConnected { worker, reply })
            .await
            .unwrap_or(false)
    }

    /// Connected peers silent longer than `timeout_ms` as of `now_ms`.
    pub async fn stale_workers(&self, now_ms: i64, timeout_ms: i64) -> Vec<String> {
        self.call(|reply| SchedulerMsg::StaleWorkers {
            now_ms,
            timeout_ms,
            reply,
        })
        .await
        .unwrap_or_default()
    }

    pub async fn worker_authorized_for_project(&self, worker_id: &str, project: ProjectId) -> bool {
        let worker = worker_id.to_owned();
        self.call(|reply| SchedulerMsg::AuthorizedFor {
            worker,
            project,
            reply,
        })
        .await
        .unwrap_or(false)
    }

    /// Register a new connection and open its `worker_connection` row.
    pub async fn register_worker(
        &self,
        worker_id: &str,
        capabilities: GradientCapabilities,
        authorized_peers: HashSet<ProjectId>,
        session: Arc<dyn SessionPort>,
    ) -> Result<Registered> {
        let caps_json = serde_json::to_value(&capabilities).unwrap_or(serde_json::Value::Null);
        let registered = self
            .reattach_worker(worker_id, capabilities, authorized_peers, session, Vec::new())
            .await?;
        self.record_worker_connection(worker_id, caps_json).await;
        Ok(registered)
    }

    /// Register without touching the DB: the session is already connected and
    /// the scheduler's state was rebuilt, so only the in-memory view is missing.
    pub async fn reattach_worker(
        &self,
        worker_id: &str,
        capabilities: GradientCapabilities,
        authorized_peers: HashSet<ProjectId>,
        session: Arc<dyn SessionPort>,
        active: Vec<(String, PendingJob)>,
    ) -> Result<Registered> {
        let registration = Registration {
            worker: worker_id.to_owned(),
            capabilities,
            authorized_peers,
            session,
            active,
        };
        self.call(|reply| SchedulerMsg::Register(registration, reply))
            .await
    }

    /// Resolve the worker's owning project from `worker_registration`, record it
    /// for sample attribution, and open a `worker_connection` row.
    async fn record_worker_connection(&self, worker_id: &str, capabilities: serde_json::Value) {
        let reg = gradient_entity::worker_registration::Entity::find()
            .filter(gradient_entity::worker_registration::Column::WorkerId.eq(worker_id))
            .order_by_asc(gradient_entity::worker_registration::Column::CreatedAt)
            .one(&self.state.worker_db)
            .await;
        let Ok(Some(reg)) = reg else {
            return;
        };
        let _ = self
            .cast(SchedulerMsg::SetWorkerProject {
                worker: worker_id.to_owned(),
                project: reg.peer_id,
            })
            .await;
        let conn = gradient_entity::worker_connection::Model {
            id: gradient_entity::ids::WorkerConnectionId::now_v7(),
            worker_id: worker_id.to_string(),
            project: reg.peer_id,
            display_name: reg.display_name,
            connected_at: gradient_types::now(),
            capabilities,
            ..Default::default()
        }
        .into_active_model();

        if let Err(e) = gradient_entity::worker_connection::Entity::insert(conn)
            .exec(&self.state.worker_db)
            .await
        {
            warn!(error = %e, %worker_id, "failed to insert worker_connection");
        }
        let _ = self
            .state
            .board_events
            .send(crate::BoardEvent::WorkerConnected {
                project: reg.peer_id.into(),
                worker_id: worker_id.to_owned(),
            });
    }

    /// Stamp `disconnected_at` on the worker's latest open `worker_connection`.
    async fn close_worker_connection(&self, worker_id: &str) {
        let conn = gradient_entity::worker_connection::Entity::find()
            .filter(gradient_entity::worker_connection::Column::WorkerId.eq(worker_id))
            .filter(gradient_entity::worker_connection::Column::DisconnectedAt.is_null())
            .order_by_desc(gradient_entity::worker_connection::Column::ConnectedAt)
            .one(&self.state.worker_db)
            .await;
        if let Ok(Some(conn)) = conn {
            let mut am = conn.into_active_model();
            am.disconnected_at = Set(Some(gradient_types::now()));
            if let Err(e) = am.update(&self.state.worker_db).await {
                warn!(error = %e, %worker_id, "failed to close worker_connection");
            }
        }
    }

    pub async fn update_authorized_peers(
        &self,
        worker_id: &str,
        authorized_peers: HashSet<ProjectId>,
    ) {
        let worker = worker_id.to_owned();
        let _ = self
            .call(|reply| SchedulerMsg::UpdatePeers {
                worker,
                peers: authorized_peers,
                reply,
            })
            .await;
        debug!(%worker_id, "authorized peers updated");
    }

    /// Abort the worker's active jobs that belong to `revoked_peers`; they go
    /// back to pending and every other session is offered them.
    pub async fn abort_project_jobs_on_worker(
        &self,
        worker_id: &str,
        revoked_peers: &HashSet<ProjectId>,
    ) {
        if revoked_peers.is_empty() {
            return;
        }
        let worker = worker_id.to_owned();
        let revoked = revoked_peers.clone();
        let aborted = self
            .call(|reply| SchedulerMsg::RevokePeers {
                worker,
                revoked,
                reply,
            })
            .await
            .unwrap_or(0);
        if aborted > 0 {
            info!(%worker_id, aborted, "aborted jobs for revoked project(s) on worker");
        }
    }

    pub async fn request_reauth(&self, worker_id: &str) {
        let _ = self
            .cast(SchedulerMsg::Reauth {
                worker: worker_id.to_owned(),
            })
            .await;
    }

    pub async fn update_worker_capabilities(&self, worker_id: &str, caps: WorkerCapabilities) {
        let worker = worker_id.to_owned();
        let _ = self
            .call(|reply| SchedulerMsg::UpdateCapabilities {
                worker,
                caps,
                reply,
            })
            .await;
        debug!(%worker_id, "worker capabilities updated");
        self.kick_dispatch();
    }

    pub async fn update_worker_metrics(&self, worker_id: &str, metrics: WorkerMetrics) {
        let worker = worker_id.to_owned();
        let _ = self
            .call(|reply| SchedulerMsg::UpdateMetrics {
                worker,
                metrics,
                reply,
            })
            .await;
    }

    pub async fn unregister_worker(&self, worker_id: &str) {
        self.close_worker_connection(worker_id).await;
        let worker = worker_id.to_owned();
        let requeued = self
            .call(|reply| SchedulerMsg::Unregister { worker, reply })
            .await
            .unwrap_or_default();
        build::requeue_orphaned_jobs(&self.state, &requeued).await;
        let _ = self
            .state
            .board_events
            .send(crate::BoardEvent::WorkerDisconnected {
                worker_id: worker_id.to_owned(),
            });
        self.kick_dispatch();
    }

    /// Reconcile each in-flight evaluation's status against the connected pool.
    pub async fn reconcile_waiting_state(&self) -> Result<()> {
        let workers = self.board_workers().await;
        let eval_capable = workers.iter().filter(|w| w.capabilities.eval).count();
        let fetch_capable = workers.iter().filter(|w| w.capabilities.fetch).count();
        let caps: Vec<(Vec<String>, Vec<String>)> = workers
            .into_iter()
            .map(|w| (w.architectures, w.system_features))
            .collect();
        let draining = self.draining.load(std::sync::atomic::Ordering::Relaxed);
        build::reconcile_waiting_state(&self.state, &caps, eval_capable, fetch_capable, draining)
            .await
    }

    /// Every connected worker, including the sampling fields the API masks.
    pub async fn board_workers(&self) -> Vec<crate::WorkerInfo> {
        self.call(|reply| SchedulerMsg::Workers { reply })
            .await
            .unwrap_or_default()
    }

    pub async fn mark_worker_draining(&self, worker_id: &str) {
        let worker = worker_id.to_owned();
        let _ = self
            .call(|reply| SchedulerMsg::MarkDraining { worker, reply })
            .await;
        info!(%worker_id, "worker marked draining");
    }
}
