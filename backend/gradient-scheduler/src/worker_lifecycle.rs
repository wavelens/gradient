/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::events::worker;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
};
use tracing::{debug, info, warn};

use gradient_types::ids::ProjectId;
use gradient_wire::types::{BuildStage, GradientCapabilities};

use crate::Scheduler;
use crate::actor::{Registered, Registration, SchedulerMsg, WorkerCapabilities, WorkerMetrics};
use crate::build;
use crate::jobs::{AwaitingReclaim, reclaim_grace};
use crate::waiting_state::WaitingRefresh;
use gradient_pool::session_port::SessionPort;

pub(crate) async fn record_worker_sample(
    db: &impl sea_orm::ConnectionTrait,
    info: &gradient_pool::WorkerInfo,
) {
    let sample = gradient_entity::worker_sample::Model {
        id: gradient_entity::ids::WorkerSampleId::now_v7(),
        worker_id: info.id.clone(),
        at: gradient_types::now(),
        cpu_usage_pct: info.cpu_usage_pct,
        ram_free_mb: info.ram_free_mb.map(|v| v as i64),
        ram_total_mb: Some(info.ram_total_mb as i64),
        disk_speed_mbps: info.disk_speed_mbps,
        upload_speed_mbps: info.upload_speed_mbps,
        download_speed_mbps: info.download_speed_mbps,
        assigned_jobs: i32::try_from(info.assigned_job_count).unwrap_or(i32::MAX),
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

    pub async fn register_worker(
        &self,
        worker_id: &str,
        capabilities: GradientCapabilities,
        authorized_peers: HashSet<ProjectId>,
        session: Arc<dyn SessionPort>,
        reclaims_jobs: bool,
    ) -> Result<Registered> {
        let caps_json = serde_json::to_value(&capabilities).unwrap_or(serde_json::Value::Null);
        let projects = authorized_peers.iter().copied().collect();
        self.workers.forget(worker_id);
        let mut registered = self
            .reattach_worker(
                worker_id,
                capabilities,
                authorized_peers,
                session,
                Vec::new(),
                reclaims_jobs,
            )
            .await?;
        if !reclaims_jobs {
            self.close_unclaimed_assignments(worker_id).await;
        }
        self.requeue_held(std::mem::take(&mut registered.unreclaimed))
            .await;
        self.record_worker_connection(worker_id, caps_json).await;
        self.db.events.publish(worker::Connected {
            worker_id: worker_id.to_owned(),
            projects,
        });
        Ok(registered)
    }

    /// The cutoff is keeping this from stealing another closer's row. A row this process handed out
    /// is closed by the worker's terminal report on a detached task. A blanket close by worker id
    /// would rewrite a `Completed` dispatch as `Abandoned` across a deploy.
    async fn close_unclaimed_assignments(&self, worker_id: &str) {
        match gradient_db::scheduling::assignment_record::abandon_open_assignments_for_worker(
            &self.db.worker_db,
            worker_id,
            self.started_at.naive_utc(),
        )
        .await
        {
            Ok(rows) if rows > 0 => {
                info!(%worker_id, rows, "closed dispatch rows a registering worker no longer runs");
            }
            Ok(_) => {}
            Err(e) => {
                warn!(error = %e, %worker_id, "failed to close the worker's open dispatch rows")
            }
        }
    }

    pub async fn reattach_worker(
        &self,
        worker_id: &str,
        capabilities: GradientCapabilities,
        authorized_peers: HashSet<ProjectId>,
        session: Arc<dyn SessionPort>,
        active: Vec<crate::jobs::Reattached>,
        reclaims_jobs: bool,
    ) -> Result<Registered> {
        let reports = self.workers.reports(worker_id);
        let registration = Registration {
            worker: worker_id.to_owned(),
            capabilities,
            authorized_peers: reports.authorized_peers.unwrap_or(authorized_peers),
            session,
            active,
            reclaims_jobs,
        };
        let registered = self
            .call(|reply| SchedulerMsg::Register(registration, reply))
            .await?;
        if let Some(caps) = reports.capabilities {
            self.update_worker_capabilities(worker_id, caps).await;
        }
        if let Some(metrics) = reports.metrics {
            self.update_worker_metrics(worker_id, metrics).await;
        }

        Ok(registered)
    }

    async fn record_worker_connection(&self, worker_id: &str, capabilities: serde_json::Value) {
        let conn = gradient_entity::worker_connection::Model {
            id: gradient_entity::ids::WorkerConnectionId::now_v7(),
            worker_id: worker_id.to_string(),
            connected_at: gradient_types::now(),
            capabilities,
            ..Default::default()
        }
        .into_active_model();

        if let Err(e) = gradient_entity::worker_connection::Entity::insert(conn)
            .exec(&self.db.worker_db)
            .await
        {
            warn!(error = %e, %worker_id, "failed to insert worker_connection");
        }
    }

    async fn close_worker_connection(&self, worker_id: &str) {
        let conn = gradient_entity::worker_connection::Entity::find()
            .filter(gradient_entity::worker_connection::Column::WorkerId.eq(worker_id))
            .filter(gradient_entity::worker_connection::Column::DisconnectedAt.is_null())
            .order_by_desc(gradient_entity::worker_connection::Column::ConnectedAt)
            .one(&self.db.worker_db)
            .await;
        if let Ok(Some(conn)) = conn {
            let mut am = conn.into_active_model();
            am.disconnected_at = Set(Some(gradient_types::now()));
            if let Err(e) = am.update(&self.db.worker_db).await {
                warn!(error = %e, %worker_id, "failed to close worker_connection");
            }
        }
    }

    pub async fn update_authorized_peers(
        &self,
        worker_id: &str,
        authorized_peers: HashSet<ProjectId>,
    ) {
        self.workers.update(worker_id, |r| {
            r.authorized_peers = Some(authorized_peers.clone())
        });
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
        let revoked_jobs = self
            .call(|reply| SchedulerMsg::RevokePeers {
                worker,
                revoked,
                reply,
            })
            .await
            .unwrap_or_default();
        if revoked_jobs.is_empty() {
            return;
        }
        info!(%worker_id, aborted = revoked_jobs.len(), "aborted jobs for revoked project(s) on worker");
        build::requeue_orphaned_jobs(
            &self.db,
            &self.graph,
            &revoked_jobs,
            &Default::default(),
            gradient_graph::OrphanCause::AccessRevoked,
        )
        .await;
        self.build_kick.kick();
    }

    pub async fn request_reauth(&self, worker_id: &str) {
        let _ = self
            .cast(SchedulerMsg::Reauth {
                worker: worker_id.to_owned(),
            })
            .await;
    }

    pub async fn update_worker_capabilities(&self, worker_id: &str, caps: WorkerCapabilities) {
        self.workers
            .update(worker_id, |r| r.capabilities = Some(caps.clone()));
        let worker = worker_id.to_owned();
        let _ = self
            .call(|reply| SchedulerMsg::UpdateCapabilities {
                worker,
                caps,
                reply,
            })
            .await;
        debug!(%worker_id, "worker capabilities updated");
        self.build_kick.kick();
    }

    pub async fn update_worker_metrics(&self, worker_id: &str, metrics: WorkerMetrics) {
        self.workers
            .update(worker_id, |r| r.metrics = Some(metrics.clone()));
        let worker = worker_id.to_owned();
        let _ = self
            .call(|reply| SchedulerMsg::UpdateMetrics {
                worker,
                metrics,
                reply,
            })
            .await;
    }

    pub async fn enter_build_stage(&self, worker_id: &str, job_id: &str, stage: BuildStage) {
        let _ = self
            .cast(SchedulerMsg::EnterStage {
                worker: worker_id.to_owned(),
                job_id: job_id.to_owned(),
                stage,
            })
            .await;
    }

    pub async fn unregister_worker(&self, worker_id: &str) {
        let grace = self.reclaim_grace_of(worker_id);
        self.workers.forget(worker_id);
        self.close_worker_connection(worker_id).await;
        let worker = worker_id.to_owned();
        let gone = self
            .call(|reply| SchedulerMsg::Unregister {
                worker,
                grace,
                reply,
            })
            .await
            .unwrap_or_default();
        build::requeue_orphaned_jobs(
            &self.db,
            &self.graph,
            &gone.requeued,
            &gone.observed_peak_ram_mb,
            gradient_graph::OrphanCause::WorkerLost,
        )
        .await;
        for lost in gone.cluster_members {
            let report = crate::cluster::MemberReport::Lost { job: lost.job };
            if let Err(e) = self.on_cluster_member_closed(&lost.key, report).await {
                warn!(error = %e, %worker_id, key = %lost.key, "settling a lost cluster member failed");
            }
        }
        self.reassign_input_fetches_of(worker_id).await;
        self.db.events.publish(worker::Disconnected {
            worker_id: worker_id.to_owned(),
        });
        self.build_kick.kick();
    }

    fn reclaim_grace_of(&self, worker_id: &str) -> Duration {
        let configured = Duration::from_secs(self.db.config.proto.assignment_reclaim_grace_secs);
        let exits_at = self
            .workers
            .reports(worker_id)
            .metrics
            .and_then(|m| m.exits_at);
        reclaim_grace(configured, exits_at, Instant::now())
    }

    pub async fn requeue_unreclaimed(&self, now: Instant) {
        let unreclaimed = self
            .call(|reply| SchedulerMsg::TakeUnreclaimed { now, reply })
            .await
            .unwrap_or_default();
        self.requeue_held(unreclaimed).await;
    }

    pub(crate) async fn requeue_held(&self, held: Vec<(String, AwaitingReclaim)>) {
        if held.is_empty() {
            return;
        }
        let observed_peak_ram_mb: HashMap<String, u64> = held
            .iter()
            .filter_map(|(key, held)| Some((key.clone(), held.observed_peak_ram_mb?)))
            .collect();
        let keys: Vec<String> = held.into_iter().map(|(key, _)| key).collect();
        info!(jobs = keys.len(), "re-queuing jobs no worker reclaimed");
        build::requeue_orphaned_keys(
            &self.db,
            &self.graph,
            &keys,
            &observed_peak_ram_mb,
            gradient_graph::OrphanCause::WorkerLost,
        )
        .await;
        self.build_kick.kick();
    }

    pub async fn refresh_waiting_state(&self) -> Result<()> {
        let workers = self.board_workers().await;
        let draining = self.draining.is_draining().await;
        let unbuildables = WaitingRefresh {
            db: &self.db,
            graph: &self.graph,
            memo: &self.scoring.assessments,
        }
        .run(&workers, draining)
        .await?;
        for unbuildable in unbuildables {
            self.abort_unbuildable_evaluation(unbuildable).await;
        }

        Ok(())
    }

    pub async fn board_workers(&self) -> Vec<gradient_pool::WorkerInfo> {
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
