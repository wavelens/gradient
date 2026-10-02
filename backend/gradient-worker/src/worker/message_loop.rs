/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_util::sync::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use gradient_wire::messages::{
    ArchivedServerMessage, BuildFailureKind, CachedPath, ClientMessage, ClusterAddress,
    ClusterMembership, ClusterPeer, Job, JobCandidate, JobKind, ServerMessage,
};
use gradient_wire::session::frame::{Frame, Inbound};
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

use crate::config::WorkerConfig;
use crate::executor::JobExecutor;
use crate::executor::abort_true;
use crate::executor::failure::JobAborted;
use crate::executor::timeline::{JobTimeline, TimelineSnapshot};
use crate::proto::credentials::CredentialStore;
use crate::proto::job::JobUpdater;
use crate::proto::scorer::JobScorer;
use crate::shutdown::Shutdown;
use gradient_worker_client::connection::{ProtoReader, ProtoWriter};
use gradient_worker_client::correlation::{AssignmentHandle, CacheWaiters, KnownDerivationWaiters};

use super::cluster::{ClusterChannels, ClusterHolds, HeldJob};
use super::scoring::spawn_scoring_task;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LoopEnd {
    pub(super) draining: bool,
    pub(super) refused: bool,
}

pub(super) async fn run_message_loop(
    mut state: MessageLoopState,
    mut reader: ProtoReader,
    shutdown: Shutdown,
) -> Result<LoopEnd> {
    let mut done_rx = state
        .done_rx
        .take()
        .expect("dispatch loop runs once per connection");

    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(10));
    heartbeat.tick().await;

    let mut local_drain = false;

    info!("entering dispatch loop");

    loop {
        tokio::select! {
            biased;

            _ = shutdown.abort_requested() => {
                warn!(active = state.jobs.len(), "stop requested; abandoning in-flight jobs");
                break;
            }

            _ = shutdown.drain_requested(), if !local_drain => {
                local_drain = true;
                state.begin_drain().await?;
                if state.jobs.is_idle() {
                    break;
                }
                info!(
                    active = state.jobs.len(),
                    "draining: finishing in-flight jobs before shutdown"
                );
            }

            Some((job_id, result)) = done_rx.recv() => {
                state.on_job_done(job_id, result).await?;
                if local_drain && state.jobs.is_idle() {
                    info!("drained: every in-flight job has reported");
                    break;
                }
            }

            _ = heartbeat.tick() => {
                state.on_heartbeat().await?;
            }

            inbound = reader.recv() => {
                let Some(inbound) = inbound else {
                    info!("server closed connection");
                    break;
                };
                let started = std::time::Instant::now();
                let kind = inbound.variant_name();
                let result = match state.nar_recv.absorb(inbound).await {
                    None => Ok(()),
                    Some(Inbound::Bulk(frame)) => state.route_bulk(frame).await,
                    Some(Inbound::Control(msg)) => state.route(msg).await,
                };
                let elapsed_ms = started.elapsed().as_millis();
                if elapsed_ms > 1_000 {
                    warn!(kind, elapsed_ms, "slow message dispatch - loop was blocked");
                }
                result?;
            }
        }
    }

    // Running jobs are detached from this loop and can never report over the dead writer.
    // Aborting them is preventing a double execution after the reconnect.
    // The server is re-queuing the orphaned jobs on its side.
    for (_job_id, job) in state.jobs.running.drain() {
        let _ = job.abort.send(true);
    }

    Ok(LoopEnd {
        draining: state.draining && !local_drain,
        refused: state.refused,
    })
}

struct ActiveJob {
    kind: JobKind,
    assignment_id: AssignmentHandle,
    abort: watch::Sender<bool>,
    timeline: Arc<JobTimeline>,
    cluster: Option<String>,
}

struct JobRegistry {
    running: HashMap<String, ActiveJob>,
    done_tx: mpsc::UnboundedSender<(String, Result<()>)>,
}

impl JobRegistry {
    fn active(&self, kind: JobKind) -> u32 {
        self.running.values().filter(|j| j.kind == kind).count() as u32
    }

    fn len(&self) -> usize {
        self.running.len()
    }

    fn is_idle(&self) -> bool {
        self.running.is_empty()
    }

    fn register(
        &mut self,
        job_id: String,
        kind: JobKind,
        assignment_id: String,
        cluster: Option<String>,
    ) -> (AssignmentHandle, watch::Receiver<bool>, Arc<JobTimeline>) {
        let assignment_id = AssignmentHandle::new(assignment_id);
        let (abort, abort_rx) = watch::channel(false);
        let timeline = JobTimeline::new();
        self.running.insert(
            job_id,
            ActiveJob {
                kind,
                assignment_id: assignment_id.clone(),
                abort,
                timeline: Arc::clone(&timeline),
                cluster,
            },
        );
        (assignment_id, abort_rx, timeline)
    }

    fn readopt(&self, job_id: &str, assignment_id: &str) -> bool {
        match self.running.get(job_id) {
            Some(job) => {
                job.assignment_id.set(assignment_id.to_owned());
                true
            }
            None => false,
        }
    }

    fn abort(&self, job_id: &str) -> bool {
        match self.running.get(job_id) {
            Some(job) => job.abort.send(true).is_ok(),
            None => false,
        }
    }

    fn abort_attempt(&self, attempt: &str) -> usize {
        self.running
            .values()
            .filter(|j| j.cluster.as_deref() == Some(attempt))
            .filter(|j| j.abort.send(true).is_ok())
            .count()
    }

    fn finish(&mut self, job_id: &str) -> Option<ActiveJob> {
        self.running.remove(job_id)
    }
}

pub(super) struct MessageLoopState {
    writer: ProtoWriter,
    cache_waiters: CacheWaiters,
    known_derivation_waiters: KnownDerivationWaiters,
    nar_recv: gradient_worker_client::nar_recv::NarReceiver,
    eval_cache_recv: crate::proto::eval_cache_recv::EvalCacheReceiver,
    uploads: gradient_worker_client::upload::UploadClient,
    jobs: JobRegistry,
    holds: ClusterHolds,
    channels: ClusterChannels,
    done_rx: Option<mpsc::UnboundedReceiver<(String, Result<()>)>>,
    max_eval: u32,
    max_build: u32,
    credentials: CredentialStore,
    scorer: JobScorer,
    executor: JobExecutor,
    config: WorkerConfig,
    draining: bool,
    refused: bool,
}

impl MessageLoopState {
    pub(super) fn new(
        writer: ProtoWriter,
        config: WorkerConfig,
        executor: JobExecutor,
        scorer: JobScorer,
        credentials: CredentialStore,
    ) -> Self {
        let nar_recv = match gradient_storage::PartialStore::new(config.nar_partial_dir()) {
            Ok(store) => gradient_worker_client::nar_recv::NarReceiver::with_partial_store(store),
            Err(e) => {
                warn!(error = %e, "failed to init NAR partial dir; downloads will not resume");
                gradient_worker_client::nar_recv::NarReceiver::new()
            }
        };
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        let channels = ClusterChannels::new(writer.clone());
        let uploads = gradient_worker_client::upload::UploadClient::new(
            writer.clone(),
            config.nar.max_concurrent_uploads as usize,
        );
        Self {
            uploads,
            writer,
            cache_waiters: Arc::new(Mutex::new(HashMap::new())),
            known_derivation_waiters: Arc::new(Mutex::new(HashMap::new())),
            nar_recv,
            eval_cache_recv: crate::proto::eval_cache_recv::EvalCacheReceiver::new(),
            jobs: JobRegistry {
                running: HashMap::new(),
                done_tx,
            },
            holds: ClusterHolds::default(),
            channels,
            done_rx: Some(done_rx),
            max_eval: config.eval.max_concurrent,
            max_build: config.build.max_concurrent,
            credentials,
            scorer,
            executor,
            config,
            draining: false,
            refused: false,
        }
    }

    async fn begin_drain(&mut self) -> Result<()> {
        self.draining = true;
        let held = self.holds.release_all();
        self.report_released(held, "worker draining").await?;
        self.writer.send(ClientMessage::Draining).await
    }

    fn occupied(&self, kind: JobKind) -> u32 {
        self.jobs.active(kind.clone()) + self.holds.held(&kind)
    }

    fn max_for(&self, kind: JobKind) -> u32 {
        match kind {
            JobKind::Flake => self.max_eval,
            JobKind::Build => self.max_build,
        }
    }

    async fn route(&mut self, msg: ServerMessage) -> Result<()> {
        match msg {
            ServerMessage::JobListChunk {
                candidates,
                is_final,
            } => {
                self.on_job_list_chunk(candidates, is_final);
            }
            ServerMessage::JobOffer { candidates } => {
                self.on_job_offer(candidates);
            }
            ServerMessage::AssignJob {
                job_id,
                assignment_id,
                job,
                cluster,
            } => {
                self.on_assign_job(job_id, assignment_id, job, cluster.map(|m| *m))
                    .await?;
            }
            ServerMessage::StartCluster { attempt, roster } => {
                self.on_start_cluster(attempt, roster).await?;
            }
            ServerMessage::ClusterSignal {
                attempt,
                from,
                payload,
            } => {
                if !self.channels.deliver(&attempt, from, payload) {
                    debug!(%attempt, "ClusterSignal for an attempt with no running member - dropped");
                }
            }
            ServerMessage::AbortCluster { attempt, reason } => {
                self.on_abort_cluster(attempt, reason).await?;
            }
            ServerMessage::AbortJob { job_id, reason } => {
                self.on_abort_job(job_id, reason).await?;
            }
            ServerMessage::Credential { kind, data } => {
                self.on_credential(kind, data);
            }
            ServerMessage::Draining => {
                info!("server is draining; finishing in-flight work then disconnecting");
                self.draining = true;
            }
            ServerMessage::Error { code, message } => {
                error!(code, %message, "protocol error from server");
            }
            ServerMessage::Reject { code, reason } => {
                warn!(code, %reason, "server refused the session");
                self.refused = true;
            }
            ServerMessage::InitAck { .. } | ServerMessage::Authenticate { .. } => {
                warn!("unexpected handshake message in dispatch loop - ignoring");
            }
            ServerMessage::AuthChallenge { peers } => {
                self.on_auth_challenge(peers).await?;
            }
            ServerMessage::AuthUpdate {
                authorized_peers,
                failed_peers,
            } => {
                self.on_auth_update(authorized_peers, failed_peers);
            }
            ServerMessage::CacheStatus { query_id, cached } => {
                self.on_cache_status(query_id, cached);
            }
            ServerMessage::CacheError { query_id, message } => {
                self.on_cache_error(query_id, message);
            }
            msg @ (ServerMessage::UploadGrant { .. } | ServerMessage::UploadCommitted { .. }) => {
                self.uploads.deliver(msg);
            }
            ServerMessage::KnownDerivations { query_id, known } => {
                self.on_known_derivations(query_id, known);
            }
            ServerMessage::EvalCachePullResult { job_id, outcome } => {
                self.eval_cache_recv.deliver_pull_result(&job_id, outcome);
            }
            ServerMessage::NarPush { .. }
            | ServerMessage::EvalCacheChunk { .. }
            | ServerMessage::NarStreamHeader { .. }
            | ServerMessage::NarUnavailable { .. }
            | ServerMessage::NarAbort { .. } => {
                warn!("a NAR or bulk frame reached the control dispatch");
            }
        }
        Ok(())
    }

    async fn route_bulk(&mut self, frame: Frame<ServerMessage>) -> Result<()> {
        match frame.archived() {
            ArchivedServerMessage::EvalCacheChunk {
                job_id,
                data,
                offset,
                is_final,
            } => {
                self.eval_cache_recv.deliver_pull_chunk(
                    job_id.as_str(),
                    data.as_slice(),
                    offset.to_native(),
                    *is_final,
                );
            }
            _ => warn!("non-bulk variant routed to the bulk lane"),
        }
        Ok(())
    }

    async fn on_job_done(&mut self, job_id: String, result: Result<()>) -> Result<()> {
        let job = self
            .jobs
            .finish(&job_id)
            .expect("a job task is registered before it is spawned");
        if let Some(attempt) = &job.cluster {
            self.channels.close(attempt);
        }
        gradient_worker_client::correlation::forget_cache_waiters_for_job(
            &self.cache_waiters,
            &job_id,
        );
        gradient_worker_client::correlation::forget_known_derivation_waiters_for_job(
            &self.known_derivation_waiters,
            &job_id,
        );
        self.nar_recv.forget_job(&job_id);
        self.eval_cache_recv.forget_job(&job_id);
        self.uploads.forget_job(&job_id);
        self.credentials.clear();

        let completed_kind = job.kind;
        let assignment_id = job.assignment_id.get();
        let dropped_spans = job.timeline.dropped();
        let TimelineSnapshot { spans, elapsed_ms } = job.timeline.snapshot();
        if dropped_spans > 0 {
            debug!(%job_id, dropped_spans, "phase timeline hit its span cap");
        }

        match result {
            Ok(()) => {
                info!(%job_id, phases = spans.len(), "job completed");
                self.writer
                    .send(ClientMessage::JobCompleted {
                        job_id,
                        assignment_id,
                        spans,
                        elapsed_ms,
                    })
                    .await?;
            }
            Err(e) => {
                let error_chain = format!("{e:#}");
                let (kind, missing_paths) = crate::executor::failure::wire_failure(&e);
                error!(%job_id, error = %error_chain, ?kind, phases = spans.len(), "job failed");
                self.writer
                    .send(ClientMessage::JobFailed {
                        job_id,
                        assignment_id,
                        error: error_chain,
                        kind,
                        missing_paths,
                        spans,
                        elapsed_ms,
                    })
                    .await?;
            }
        }

        if !self.draining {
            let kind = completed_kind;
            if self.occupied(kind.clone()) < self.max_for(kind.clone()) {
                self.writer.send(ClientMessage::RequestJob { kind }).await?;
            }
        }

        Ok(())
    }

    /// A failed send is ending the session.
    /// The control lane is only staying full past its timeout when the peer has stopped reading.
    /// A peer that stalled without closing is never ending the read half.
    /// Warning and carrying on left the worker building jobs it could no longer report.
    async fn on_heartbeat(&mut self) -> Result<()> {
        send_live_metrics(&self.writer);
        let expired = self.holds.expired(std::time::Instant::now());
        self.report_released(expired, "cluster start timed out")
            .await?;
        if self.draining {
            return Ok(());
        }
        let active_eval = self.occupied(JobKind::Flake);
        let active_build = self.occupied(JobKind::Build);
        let want_eval = active_eval < self.max_eval;
        let want_build = active_build < self.max_build;
        debug!(
            active_eval,
            max_eval = self.max_eval,
            active_build,
            max_build = self.max_build,
            request_eval = want_eval,
            request_build = want_build,
            "heartbeat tick"
        );
        if want_eval {
            self.writer
                .send(ClientMessage::RequestJob {
                    kind: JobKind::Flake,
                })
                .await
                .context("heartbeat RequestJob Flake")?;
        }
        if want_build {
            self.writer
                .send(ClientMessage::RequestJob {
                    kind: JobKind::Build,
                })
                .await
                .context("heartbeat RequestJob Build")?;
        }

        Ok(())
    }

    fn on_job_list_chunk(&mut self, cands: Vec<JobCandidate>, is_final: bool) {
        debug!(count = cands.len(), is_final, "received job list chunk");
        if self.draining {
            return;
        }
        spawn_scoring_task(
            self.scorer,
            Arc::clone(&self.executor.store),
            self.writer.clone(),
            cands,
            is_final,
            Vec::new(),
        );
    }

    #[tracing::instrument(level = "debug", skip_all, fields(candidates = cands.len()))]
    fn on_job_offer(&mut self, cands: Vec<JobCandidate>) {
        debug!(count = cands.len(), "received job offer");
        if self.draining || cands.is_empty() {
            return;
        }
        let request_after: Vec<JobKind> = [
            (JobKind::Build, self.max_build),
            (JobKind::Flake, self.max_eval),
        ]
        .into_iter()
        .filter(|(kind, max)| self.occupied(kind.clone()) < *max)
        .map(|(kind, _)| kind)
        .collect();
        spawn_scoring_task(
            self.scorer,
            Arc::clone(&self.executor.store),
            self.writer.clone(),
            cands,
            true,
            request_after,
        );
    }

    async fn on_assign_job(
        &mut self,
        job_id: String,
        assignment_id: String,
        job: Job,
        cluster: Option<ClusterMembership>,
    ) -> Result<()> {
        if self.jobs.readopt(&job_id, &assignment_id) {
            warn!(%job_id, dispatch = %assignment_id, "job assigned again while still running; reporting under the new dispatch id");
            self.writer
                .send(ClientMessage::AssignJobResponse {
                    job_id,
                    accepted: true,
                    reason: None,
                })
                .await?;
            return Ok(());
        }

        if self.draining {
            warn!(%job_id, "rejecting assigned job - draining");
            self.writer
                .send(ClientMessage::AssignJobResponse {
                    job_id,
                    accepted: false,
                    reason: Some("worker is draining".to_owned()),
                })
                .await?;
            return Ok(());
        }

        let kind = match &job {
            Job::Flake(_) => JobKind::Flake,
            Job::Build(_) => JobKind::Build,
        };
        let active_count = self.occupied(kind.clone());
        let max = self.max_for(kind.clone());

        if active_count >= max {
            warn!(%job_id, ?kind, active = active_count, limit = max, "rejecting assigned job - at capacity");
            self.writer
                .send(ClientMessage::AssignJobResponse {
                    job_id,
                    accepted: false,
                    reason: Some(format!("at capacity ({}/{})", active_count, max)),
                })
                .await?;
            return Ok(());
        }

        if let Some(membership) = cluster {
            let attempt = membership.attempt.clone();
            let held = HeldJob {
                job_id: job_id.clone(),
                assignment_id,
                job,
                kind: kind.clone(),
                credentials: self.credentials.snapshot(),
            };
            let accepted = self.holds.hold(membership, held, std::time::Instant::now());
            info!(%job_id, %attempt, accepted, "cluster member assigned - holding its slot");
            self.writer
                .send(ClientMessage::AssignJobResponse {
                    job_id,
                    accepted,
                    reason: (!accepted)
                        .then(|| "already holds a member of this cluster attempt".to_owned()),
                })
                .await?;
            if accepted && active_count + 1 < max {
                self.writer.send(ClientMessage::RequestJob { kind }).await?;
            }
            return Ok(());
        }

        info!(%job_id, ?kind, "job assigned - accepting");
        self.writer
            .send(ClientMessage::AssignJobResponse {
                job_id: job_id.clone(),
                accepted: true,
                reason: None,
            })
            .await?;

        let credentials = self.credentials.clone();
        self.spawn_job(
            job_id.clone(),
            kind.clone(),
            assignment_id,
            job,
            None,
            credentials,
        );

        if active_count + 1 < max {
            self.writer.send(ClientMessage::RequestJob { kind }).await?;
        }

        Ok(())
    }

    fn spawn_job(
        &mut self,
        job_id: String,
        kind: JobKind,
        assignment_id: String,
        job: Job,
        cluster: Option<String>,
        credentials: CredentialStore,
    ) {
        let (assignment_id, abort_rx, timeline) =
            self.jobs
                .register(job_id.clone(), kind, assignment_id, cluster);

        let executor = self.executor.clone();
        let job_store = Arc::clone(&executor.store);
        let job_writer = self.writer.clone();
        let job_cache_waiters = Arc::clone(&self.cache_waiters);
        let job_known_derivation_waiters = Arc::clone(&self.known_derivation_waiters);
        let job_nar_recv = self.nar_recv.clone();
        let job_eval_cache_recv = self.eval_cache_recv.clone();
        let job_uploads = self.uploads.clone();
        let job_done_tx = self.jobs.done_tx.clone();
        let jid = job_id.clone();

        #[expect(
            clippy::disallowed_methods,
            reason = "reports back on done_tx and aborts through the registry"
        )]
        tokio::spawn(async move {
            let mut updater = JobUpdater::new(
                jid.clone(),
                assignment_id,
                job_writer,
                job_cache_waiters,
                job_known_derivation_waiters,
                job_nar_recv,
                job_eval_cache_recv,
                Some(job_store),
                timeline,
                job_uploads,
            );
            let result = run_job(executor, job, &mut updater, &credentials, abort_rx).await;
            let _ = job_done_tx.send((jid, result));
        });
    }

    async fn on_start_cluster(&mut self, attempt: String, roster: Vec<ClusterPeer>) -> Result<()> {
        let Some((membership, held)) = self.holds.start(&attempt) else {
            warn!(%attempt, "StartCluster for an attempt this worker does not hold - ignoring");
            return Ok(());
        };
        info!(%attempt, job_id = %held.job_id, peers = roster.len(), ?roster, "cluster started - running the held member");
        let me = ClusterAddress {
            role: membership.role,
            index: membership.index,
        };
        self.channels.open(&attempt, me, roster);
        self.spawn_job(
            held.job_id,
            held.kind,
            held.assignment_id,
            held.job,
            Some(attempt),
            held.credentials,
        );

        Ok(())
    }

    async fn on_abort_cluster(&mut self, attempt: String, reason: String) -> Result<()> {
        let held = self.holds.drop_attempt(&attempt);
        let running = self.jobs.abort_attempt(&attempt);
        self.channels.close(&attempt);
        warn!(%attempt, %reason, held = held.is_some(), running, "cluster attempt aborted by server");
        if let Some(job) = held
            && !self.draining
        {
            self.writer
                .send(ClientMessage::RequestJob { kind: job.kind })
                .await?;
        }

        Ok(())
    }

    async fn report_released(&mut self, released: Vec<HeldJob>, error: &str) -> Result<()> {
        for job in released {
            warn!(job_id = %job.job_id, %error, "releasing a held cluster member");
            self.writer
                .send(ClientMessage::JobFailed {
                    job_id: job.job_id,
                    assignment_id: job.assignment_id,
                    error: error.to_owned(),
                    kind: BuildFailureKind::Aborted,
                    missing_paths: Vec::new(),
                    spans: Vec::new(),
                    elapsed_ms: 0,
                })
                .await?;
        }

        Ok(())
    }

    async fn on_abort_job(&mut self, job_id: String, reason: String) -> Result<()> {
        warn!(%job_id, %reason, "job aborted by server");
        if let Some(held) = self.holds.drop_job(&job_id) {
            let kind = held.kind.clone();
            self.report_released(vec![held], "aborted by server")
                .await?;
            if !self.draining {
                self.writer.send(ClientMessage::RequestJob { kind }).await?;
            }
            return Ok(());
        }
        if !self.jobs.abort(&job_id) {
            debug!(%job_id, "abort for a job this session does not run");
        }
        self.uploads.cancel_job(&job_id).await;

        Ok(())
    }

    fn on_credential(&mut self, kind: gradient_wire::messages::CredentialKind, data: Vec<u8>) {
        debug!(?kind, "received credential");
        self.credentials.store(kind, data);
    }

    fn on_cache_status(&mut self, query_id: String, cached: Vec<CachedPath>) {
        let count = cached.len();
        if !gradient_worker_client::correlation::deliver_cache_reply(
            &self.cache_waiters,
            &query_id,
            Ok(cached),
        ) {
            debug!(%query_id, count, "CacheStatus arrived after waiter cleared");
        }
    }

    fn on_cache_error(&mut self, query_id: String, message: String) {
        if !gradient_worker_client::correlation::deliver_cache_reply(
            &self.cache_waiters,
            &query_id,
            Err(message),
        ) {
            debug!(%query_id, "CacheError arrived after waiter cleared");
        }
    }

    fn on_known_derivations(&mut self, query_id: String, known: Vec<String>) {
        let count = known.len();
        if !gradient_worker_client::correlation::deliver_known_derivations(
            &self.known_derivation_waiters,
            &query_id,
            known,
        ) {
            debug!(%query_id, count, "KnownDerivations arrived after waiter cleared");
        }
    }

    async fn on_auth_challenge(&mut self, peers: Vec<String>) -> Result<()> {
        debug!(
            ?peers,
            "mid-connection AuthChallenge - sending AuthResponse"
        );
        let peer_tokens = self.config.peer_tokens();
        let tokens = gradient_worker_client::connection::handshake::resolve_tokens_for_challenge(
            &peer_tokens,
            &peers,
        );
        self.writer
            .send(ClientMessage::AuthResponse { tokens })
            .await?;
        Ok(())
    }

    fn on_auth_update(
        &mut self,
        authorized_peers: Vec<String>,
        failed_peers: Vec<gradient_wire::messages::FailedPeer>,
    ) {
        info!(
            authorized = authorized_peers.len(),
            failed = failed_peers.len(),
            "auth updated"
        );
        for fp in &failed_peers {
            warn!(peer_id = %fp.peer_id, reason = %fp.reason, "peer auth failed");
        }
    }
}

fn send_live_metrics(writer: &ProtoWriter) {
    let writer = writer.clone();
    #[expect(
        clippy::disallowed_methods,
        reason = "one sample per heartbeat, never awaited"
    )]
    tokio::spawn(async move {
        let m = match tokio::task::spawn_blocking(crate::metrics::host_dynamic).await {
            Ok(m) => m,
            Err(e) => {
                debug!(error = %e, "host_dynamic sampling task failed");
                return;
            }
        };
        if let Err(e) = writer
            .send(ClientMessage::WorkerMetrics {
                cpu_usage_pct: m.cpu_usage_pct,
                ram_free_mb: m.ram_free_mb,
                disk_speed_mbps: gradient_worker_client::throughput::DISK.current(),
                network_speed_mbps: gradient_worker_client::throughput::NETWORK.current(),
            })
            .await
        {
            debug!(error = %e, "heartbeat WorkerMetrics send failed");
        }
    });
}

async fn run_job(
    executor: JobExecutor,
    job: Job,
    updater: &mut JobUpdater,
    credentials: &CredentialStore,
    abort: watch::Receiver<bool>,
) -> Result<()> {
    match job {
        Job::Flake(flake_job) => {
            let run = executor.execute_flake_job(flake_job, updater, credentials, abort.clone());
            until_aborted(run, abort).await
        }
        Job::Build(build_job) => {
            executor
                .execute_build_job(build_job, updater, credentials, abort)
                .await
        }
    }
}

async fn until_aborted(
    job: impl std::future::Future<Output = Result<()>>,
    mut abort: watch::Receiver<bool>,
) -> Result<()> {
    if *abort.borrow() {
        return Err(JobAborted("evaluation aborted by server".to_owned()).into());
    }

    tokio::select! {
        biased;
        result = job => result,
        () = abort_true(&mut abort) => {
            Err(JobAborted("evaluation aborted by server".to_owned()).into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> (JobRegistry, mpsc::UnboundedReceiver<(String, Result<()>)>) {
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        (
            JobRegistry {
                running: HashMap::new(),
                done_tx,
            },
            done_rx,
        )
    }

    #[test]
    fn a_reassigned_running_job_keeps_its_task_and_takes_the_new_assignment_id() {
        let (mut jobs, _done_rx) = registry();
        let (assignment_id, abort_rx, _timeline) = jobs.register(
            "job-1".to_owned(),
            JobKind::Build,
            "dispatch-1".to_owned(),
            None,
        );

        assert!(jobs.readopt("job-1", "dispatch-2"));

        assert_eq!(
            assignment_id.get(),
            "dispatch-2",
            "the task reports under the new id"
        );
        assert_eq!(
            jobs.active(JobKind::Build),
            1,
            "no second task is registered"
        );
        assert!(jobs.abort("job-1"));
        assert!(*abort_rx.borrow(), "the first task still hears the abort");
        assert!(
            !jobs.readopt("job-9", "dispatch-3"),
            "an unknown job is a fresh assignment"
        );
    }

    #[test]
    fn aborting_an_attempt_stops_only_its_members() {
        let (mut jobs, _done_rx) = registry();
        let (_, member, _) = jobs.register(
            "build:x".to_owned(),
            JobKind::Build,
            "dispatch-1".to_owned(),
            Some("a1".to_owned()),
        );
        let (_, single, _) = jobs.register(
            "build:y".to_owned(),
            JobKind::Build,
            "dispatch-2".to_owned(),
            None,
        );

        assert_eq!(jobs.abort_attempt("a1"), 1);

        assert!(*member.borrow(), "the member hears the abort");
        assert!(!*single.borrow(), "a single job keeps running");
    }

    #[test]
    fn finishing_a_job_hands_back_its_current_assignment_id() {
        let (mut jobs, _done_rx) = registry();
        jobs.register(
            "job-1".to_owned(),
            JobKind::Flake,
            "dispatch-1".to_owned(),
            None,
        );
        jobs.readopt("job-1", "dispatch-2");

        let job = jobs.finish("job-1").expect("the job was running");

        assert_eq!(job.assignment_id.get(), "dispatch-2");
        assert!(jobs.is_idle());
        assert!(jobs.finish("job-1").is_none());
    }

    #[tokio::test]
    async fn an_aborted_job_stops_without_reaching_a_checkpoint() {
        let (abort, abort_rx) = watch::channel(false);
        abort.send(true).expect("the job listens");

        let err = until_aborted(std::future::pending(), abort_rx)
            .await
            .expect_err("an aborted job fails");

        assert!(
            err.downcast_ref::<JobAborted>().is_some(),
            "reported as an abort, not a failure: {err:#}"
        );
    }

    #[tokio::test]
    async fn a_job_nobody_aborts_reports_its_own_result() {
        let (_abort, abort_rx) = watch::channel(false);

        let result = until_aborted(async { Err(anyhow::anyhow!("nix failed")) }, abort_rx).await;

        assert_eq!(
            format!("{:#}", result.expect_err("its own error")),
            "nix failed"
        );
    }

    #[tokio::test]
    async fn a_closed_abort_channel_does_not_stop_the_job() {
        let (abort, abort_rx) = watch::channel(false);
        drop(abort);

        let job = async {
            tokio::task::yield_now().await;
            Ok(())
        };

        until_aborted(job, abort_rx)
            .await
            .expect("the job runs to its end");
    }
}
