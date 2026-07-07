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

use gradient_types::ids::OrganizationId;
use gradient_types::proto::GradientCapabilities;

use crate::Scheduler;
use crate::build;

/// Insert a `worker_sample` time-series row for a connected worker. Best-effort;
/// skipped when the worker's owning org is unknown. Called from the heartbeat loop.
pub(crate) async fn record_worker_sample(
    db: &impl sea_orm::ConnectionTrait,
    info: &crate::WorkerInfo,
) {
    let Some(org) = info.organization else {
        return;
    };
    let sample = gradient_entity::worker_sample::Model {
        id: gradient_entity::ids::WorkerSampleId::now_v7(),
        worker_id: info.id.clone(),
        organization: org,
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
        self.worker_pool.read().await.is_connected(worker_id)
    }

    /// Clone a connected worker's `last_seen` handle so the session loop can
    /// stamp it lock-free on every inbound frame.
    pub async fn worker_last_seen(
        &self,
        worker_id: &str,
    ) -> Option<std::sync::Arc<std::sync::atomic::AtomicI64>> {
        self.worker_pool.read().await.last_seen_handle(worker_id)
    }

    /// Connected peers silent longer than `timeout_ms` as of `now_ms`.
    pub async fn stale_workers(&self, now_ms: i64, timeout_ms: i64) -> Vec<String> {
        self.worker_pool
            .read()
            .await
            .stale_worker_ids(now_ms, timeout_ms)
    }

    pub async fn worker_authorized_for_org(&self, worker_id: &str, org: OrganizationId) -> bool {
        self.worker_pool
            .read()
            .await
            .peer_auth_for(worker_id)
            .map(|a| a.contains(&org))
            .unwrap_or(false)
    }

    pub async fn register_worker(
        &self,
        worker_id: &str,
        capabilities: GradientCapabilities,
        authorized_peers: HashSet<OrganizationId>,
    ) -> (
        Arc<tokio::sync::Notify>,
        tokio::sync::mpsc::UnboundedReceiver<(String, String)>,
    ) {
        let caps_json = serde_json::to_value(&capabilities).unwrap_or(serde_json::Value::Null);
        let (notify, abort_rx) = self.worker_pool.write().await.register(
            worker_id.to_owned(),
            capabilities,
            authorized_peers,
        );
        info!(%worker_id, "worker registered");
        self.record_worker_connection(worker_id, caps_json).await;
        (notify, abort_rx)
    }

    /// Resolve the worker's owning org from `worker_registration`, cache it on
    /// the pool for sample attribution, and open a `worker_connection` row.
    async fn record_worker_connection(&self, worker_id: &str, capabilities: serde_json::Value) {
        let reg = gradient_entity::worker_registration::Entity::find()
            .filter(gradient_entity::worker_registration::Column::WorkerId.eq(worker_id))
            .order_by_asc(gradient_entity::worker_registration::Column::CreatedAt)
            .one(&self.state.worker_db)
            .await;
        let Ok(Some(reg)) = reg else {
            return;
        };
        self.worker_pool
            .write()
            .await
            .set_worker_org(worker_id, reg.peer_id);
        let conn = gradient_entity::worker_connection::Model {
            id: gradient_entity::ids::WorkerConnectionId::now_v7(),
            worker_id: worker_id.to_string(),
            organization: reg.peer_id,
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
                organization: reg.peer_id.into(),
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
        authorized_peers: HashSet<OrganizationId>,
    ) {
        self.worker_pool
            .write()
            .await
            .update_authorized_peers(worker_id, authorized_peers);
        debug!(%worker_id, "authorized peers updated");
    }

    /// Abort all active jobs on `worker_id` that belong to any of `revoked_peers`.
    /// Jobs are moved back to pending so they can be re-assigned to another worker.
    pub async fn abort_org_jobs_on_worker(
        &self,
        worker_id: &str,
        revoked_peers: &HashSet<OrganizationId>,
    ) {
        if revoked_peers.is_empty() {
            return;
        }
        let job_ids = self
            .job_tracker
            .write()
            .await
            .drain_peer_jobs_on_worker(worker_id, revoked_peers);
        if job_ids.is_empty() {
            return;
        }
        let pool = self.worker_pool.read().await;
        for job_id in &job_ids {
            pool.send_abort(
                worker_id,
                job_id.clone(),
                "org deactivated worker".to_owned(),
            );
        }
        info!(
            %worker_id,
            aborted = job_ids.len(),
            "aborted jobs for revoked org(s) on worker"
        );
        // Notify other workers that these jobs are available again.
        self.job_notify.send_modify(|g| *g = g.wrapping_add(1));
    }

    /// Signal a connected worker that its registrations have changed,
    /// triggering a server-initiated re-authentication.
    pub async fn request_reauth(&self, worker_id: &str) {
        self.worker_pool.read().await.request_reauth(worker_id);
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the WorkerCapabilities wire fields; refactor tracked in #503"
    )]
    pub async fn update_worker_capabilities(
        &self,
        worker_id: &str,
        architectures: Vec<String>,
        system_features: Vec<String>,
        max_concurrent_builds: u32,
        cpu_count: u32,
        ram_total_mb: u64,
        cpu_core_score: u32,
    ) {
        self.worker_pool.write().await.update_capabilities(
            worker_id,
            architectures,
            system_features,
            max_concurrent_builds,
            cpu_count,
            ram_total_mb,
            cpu_core_score,
        );
        debug!(%worker_id, "worker capabilities updated");
        // Capabilities just changed - a build that was previously "no worker
        // can do this" might now be servable, or vice-versa. Re-evaluate
        // every in-flight evaluation's Waiting/Building gate immediately
        // instead of waiting for the next dispatch tick.
        if let Err(e) = self.reconcile_waiting_state().await {
            warn!(error = %e, "reconcile_waiting_state after capability update failed");
        }
    }

    pub async fn update_worker_metrics(
        &self,
        worker_id: &str,
        cpu_usage_pct: f32,
        ram_free_mb: u64,
        disk_speed_mbps: Option<f32>,
        network_speed_mbps: Option<f32>,
    ) {
        self.worker_pool.write().await.update_metrics(
            worker_id,
            cpu_usage_pct,
            ram_free_mb,
            disk_speed_mbps,
            network_speed_mbps,
        );
        debug!(%worker_id, cpu_usage_pct, ram_free_mb, "worker metrics updated");
    }

    pub async fn unregister_worker(&self, worker_id: &str) {
        self.close_worker_connection(worker_id).await;
        let orphaned = self.worker_pool.write().await.unregister(worker_id);
        let requeued = self
            .job_tracker
            .write()
            .await
            .worker_disconnected(worker_id);
        let total = orphaned.len() + requeued.len();
        if total > 0 {
            info!(%worker_id, orphaned_jobs = total, "worker disconnected; jobs re-queued");
        }

        // The in-memory requeue above leaves the DB rows in a non-terminal
        // status (`Building` / mid-eval) that the dispatcher never re-selects;
        // reset them so the orphaned work actually retries.
        build::requeue_orphaned_jobs(&self.state, &requeued).await;

        let _ = self
            .state
            .board_events
            .send(crate::BoardEvent::WorkerDisconnected {
                worker_id: worker_id.to_owned(),
            });
        // A worker leaving may strand evaluations whose remaining builds
        // only it could service.
        if let Err(e) = self.reconcile_waiting_state().await {
            warn!(error = %e, "reconcile_waiting_state after worker unregister failed");
        }
    }

    /// Snapshot every connected worker's `(architectures, system_features)`
    /// plus the counts of those advertising the `eval` and `fetch`
    /// capabilities, then reconcile each in-flight evaluation's status. See
    /// [`build::reconcile_waiting_state`].
    pub async fn reconcile_waiting_state(&self) -> Result<()> {
        let workers = self.worker_pool.read().await.all_workers();
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

    /// Snapshot of every connected worker for the Job Board (includes the
    /// internal sampling fields; the API layer masks them per caller scope).
    pub async fn board_workers(&self) -> Vec<crate::WorkerInfo> {
        self.worker_pool.read().await.all_workers()
    }

    pub async fn mark_worker_draining(&self, worker_id: &str) {
        self.worker_pool.write().await.mark_draining(worker_id);
        info!(%worker_id, "worker marked draining");
    }
}
