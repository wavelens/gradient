/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `Scheduler` methods for worker connect / disconnect / capability management.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use tracing::{debug, info, warn};
use uuid::Uuid;

use gradient_core::types::proto::GradientCapabilities;

use crate::Scheduler;
use crate::build;

impl Scheduler {
    pub async fn is_worker_connected(&self, peer_id: &str) -> bool {
        self.worker_pool.read().await.is_connected(peer_id)
    }

    pub async fn register_worker(
        &self,
        peer_id: &str,
        capabilities: GradientCapabilities,
        authorized_peers: HashSet<Uuid>,
    ) -> (
        Arc<tokio::sync::Notify>,
        tokio::sync::mpsc::UnboundedReceiver<(String, String)>,
    ) {
        let (notify, abort_rx) = self.worker_pool.write().await.register(
            peer_id.to_owned(),
            capabilities,
            authorized_peers,
        );
        info!(%peer_id, "worker registered");
        (notify, abort_rx)
    }

    pub async fn update_authorized_peers(&self, peer_id: &str, authorized_peers: HashSet<Uuid>) {
        self.worker_pool
            .write()
            .await
            .update_authorized_peers(peer_id, authorized_peers);
        debug!(%peer_id, "authorized peers updated");
    }

    /// Abort all active jobs on `worker_id` that belong to any of `revoked_peers`.
    /// Jobs are moved back to pending so they can be re-assigned to another worker.
    pub async fn abort_org_jobs_on_worker(&self, worker_id: &str, revoked_peers: &HashSet<Uuid>) {
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
        self.job_notify.notify_waiters();
    }

    /// Signal a connected worker that its registrations have changed,
    /// triggering a server-initiated re-authentication.
    pub async fn request_reauth(&self, worker_id: &str) {
        self.worker_pool.read().await.request_reauth(worker_id);
    }

    pub async fn update_worker_capabilities(
        &self,
        peer_id: &str,
        architectures: Vec<String>,
        system_features: Vec<String>,
        max_concurrent_builds: u32,
    ) {
        self.worker_pool.write().await.update_capabilities(
            peer_id,
            architectures,
            system_features,
            max_concurrent_builds,
        );
        debug!(%peer_id, "worker capabilities updated");
        // Capabilities just changed — a build that was previously "no worker
        // can do this" might now be servable, or vice-versa. Re-evaluate
        // every in-flight evaluation's Waiting/Building gate immediately
        // instead of waiting for the next dispatch tick.
        if let Err(e) = self.reconcile_waiting_state().await {
            warn!(error = %e, "reconcile_waiting_state after capability update failed");
        }
    }

    pub async fn unregister_worker(&self, peer_id: &str) {
        let orphaned = self.worker_pool.write().await.unregister(peer_id);
        let tracker_orphaned = self.job_tracker.write().await.worker_disconnected(peer_id);
        let total = orphaned.len() + tracker_orphaned.len();
        if total > 0 {
            info!(%peer_id, orphaned_jobs = total, "worker disconnected; jobs re-queued");
        }
        // A worker leaving may strand evaluations whose remaining builds
        // only it could service.
        if let Err(e) = self.reconcile_waiting_state().await {
            warn!(error = %e, "reconcile_waiting_state after worker unregister failed");
        }
    }

    /// Snapshot every connected worker's `(architectures, system_features)`
    /// and reconcile each in-flight evaluation's `Building`/`Waiting` status.
    /// See [`build::reconcile_waiting_state`].
    pub async fn reconcile_waiting_state(&self) -> Result<()> {
        let caps: Vec<(Vec<String>, Vec<String>)> = self
            .worker_pool
            .read()
            .await
            .all_workers()
            .into_iter()
            .map(|w| (w.architectures, w.system_features))
            .collect();
        build::reconcile_waiting_state(&self.state, &caps).await
    }

    pub async fn mark_worker_draining(&self, peer_id: &str) {
        self.worker_pool.write().await.mark_draining(peer_id);
        info!(%peer_id, "worker marked draining");
    }
}
