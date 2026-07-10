/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Worker dispatch loop: drives `tokio::select!` over the server connection,
//! job completion, and heartbeats.
//!
//! [`run_dispatch_loop`] is the main entry point. It owns one [`DispatchState`]
//! per connection; every inbound [`ServerMessage`] is routed to a method on it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use gradient_proto::messages::{
    CachedPath, ClientMessage, Job, JobCandidate, JobKind, ServerMessage,
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::config::WorkerConfig;
use crate::connection::{ProtoReader, ProtoWriter};
use crate::executor::JobExecutor;
use crate::proto::credentials::CredentialStore;
use crate::proto::job::{CacheWaiters, JobUpdater, KnownDerivationWaiters};
use crate::proto::scorer::JobScorer;

use super::scoring::{send_score_chunks, spawn_scoring_task};

// ── Dispatch loop ─────────────────────────────────────────────────────────────

/// Returns the draining flag: `true` if the server sent `Draining` before
/// closing the connection.
pub(super) async fn run_dispatch_loop(
    mut state: DispatchState,
    mut reader: ProtoReader,
    shutdown: CancellationToken,
) -> Result<bool> {
    let mut done_rx = state
        .done_rx
        .take()
        .expect("dispatch loop runs once per connection");

    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(10));
    heartbeat.tick().await;

    info!("entering dispatch loop");

    loop {
        tokio::select! {
            biased;

            _ = shutdown.cancelled() => {
                info!("shutdown requested; exiting dispatch loop");
                break;
            }

            Some((job_id, result)) = done_rx.recv() => {
                state.on_job_done(job_id, result).await?;
            }

            _ = heartbeat.tick() => {
                state.on_heartbeat().await;
            }

            msg = reader.recv() => {
                let Some(msg) = msg else {
                    info!("server closed connection");
                    break;
                };
                let started = std::time::Instant::now();
                let kind = msg.variant_name();
                let result = state.dispatch(msg).await;
                let elapsed_ms = started.elapsed().as_millis();
                if elapsed_ms > 1_000 {
                    warn!(kind, elapsed_ms, "slow message dispatch - loop was blocked");
                }
                result?;
            }
        }
    }

    // The connection is gone; any job still running is detached from this loop
    // and can never report its result over the dead writer. Abort them so they
    // stop instead of double-executing after we reconnect - the server
    // re-queues the orphaned jobs on its side.
    for (_job_id, abort_tx) in state.jobs.abort_senders.drain() {
        let _ = abort_tx.send(true);
    }

    Ok(state.draining)
}

// ── Per-job bookkeeping ───────────────────────────────────────────────────────

/// Registry of in-flight jobs: abort channels, kinds for capacity accounting,
/// and the completion channel every spawned job reports back on.
struct JobRegistry {
    abort_senders: HashMap<String, watch::Sender<bool>>,
    job_kinds: HashMap<String, JobKind>,
    done_tx: mpsc::UnboundedSender<(String, Result<()>)>,
}

impl JobRegistry {
    fn active(&self, kind: JobKind) -> u32 {
        self.job_kinds.values().filter(|k| **k == kind).count() as u32
    }
}

// ── Per-connection state ──────────────────────────────────────────────────────

/// Owned per-connection dispatch state. Everything the message handlers need
/// lives here; the shared caches (`credentials`, `candidates`, `last_scores`)
/// are cheap `Arc` handles onto state the [`super::Worker`] keeps across
/// reconnects.
pub(super) struct DispatchState {
    writer: ProtoWriter,
    cache_waiters: CacheWaiters,
    known_derivation_waiters: KnownDerivationWaiters,
    nar_recv: crate::proto::nar_recv::NarReceiver,
    eval_cache_recv: crate::proto::eval_cache_recv::EvalCacheReceiver,
    jobs: JobRegistry,
    done_rx: Option<mpsc::UnboundedReceiver<(String, Result<()>)>>,
    max_eval: u32,
    max_build: u32,
    credentials: CredentialStore,
    candidates: Arc<Mutex<HashMap<String, JobCandidate>>>,
    last_scores: Arc<Mutex<HashMap<String, gradient_proto::messages::CandidateScore>>>,
    scorer: JobScorer,
    executor: JobExecutor,
    config: WorkerConfig,
    draining: bool,
}

impl DispatchState {
    pub(super) fn new(
        writer: ProtoWriter,
        config: WorkerConfig,
        executor: JobExecutor,
        scorer: JobScorer,
        credentials: CredentialStore,
        candidates: Arc<Mutex<HashMap<String, JobCandidate>>>,
        last_scores: Arc<Mutex<HashMap<String, gradient_proto::messages::CandidateScore>>>,
    ) -> Self {
        let nar_recv = match gradient_storage::PartialStore::new(
            config.nar_partial_dir(),
            std::time::Duration::from_secs(config.nar_partial_ttl_secs),
        ) {
            Ok(store) => crate::proto::nar_recv::NarReceiver::with_partial_store(store),
            Err(e) => {
                warn!(error = %e, "failed to init NAR partial dir; downloads will not resume");
                crate::proto::nar_recv::NarReceiver::new()
            }
        };
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        Self {
            writer,
            cache_waiters: Arc::new(Mutex::new(HashMap::new())),
            known_derivation_waiters: Arc::new(Mutex::new(HashMap::new())),
            nar_recv,
            eval_cache_recv: crate::proto::eval_cache_recv::EvalCacheReceiver::new(),
            jobs: JobRegistry {
                abort_senders: HashMap::new(),
                job_kinds: HashMap::new(),
                done_tx,
            },
            done_rx: Some(done_rx),
            max_eval: config.max_concurrent_evaluations,
            max_build: config.max_concurrent_builds,
            credentials,
            candidates,
            last_scores,
            scorer,
            executor,
            config,
            draining: false,
        }
    }

    fn max_for(&self, kind: JobKind) -> u32 {
        match kind {
            JobKind::Flake => self.max_eval,
            JobKind::Build => self.max_build,
        }
    }

    /// Route `msg` to the appropriate handler method.
    async fn dispatch(&mut self, msg: ServerMessage) -> Result<()> {
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
            ServerMessage::RevokeJob { job_ids } => {
                self.on_revoke_job(job_ids);
            }
            ServerMessage::AssignJob { job_id, job } => {
                self.on_assign_job(job_id, job).await?;
            }
            ServerMessage::AbortJob { job_id, reason } => {
                self.on_abort_job(job_id, reason);
            }
            ServerMessage::Credential { kind, data } => {
                self.on_credential(kind, data);
            }
            ServerMessage::NarStreamHeader {
                job_id,
                store_path,
                total_bytes,
                stream_token,
            } => {
                self.nar_recv
                    .note_header(&job_id, &store_path, total_bytes, &stream_token);
            }
            ServerMessage::NarPushResume {
                job_id,
                store_path,
                received_bytes,
            } => {
                self.nar_recv
                    .resolve_push(&job_id, &store_path, received_bytes);
            }
            ServerMessage::NarPush {
                job_id,
                store_path,
                data,
                offset,
                is_final,
            } => {
                self.on_nar_push(job_id, store_path, data, offset, is_final)
                    .await;
            }
            ServerMessage::NarUnavailable {
                job_id,
                store_path,
                reason,
            }
            | ServerMessage::NarAbort {
                job_id,
                store_path,
                reason,
            } => {
                warn!(%job_id, %store_path, %reason, "server cannot deliver NAR");
                self.nar_recv.fail(&job_id, &store_path, reason);
            }
            ServerMessage::RequestAllScores => {
                self.on_request_all_scores().await;
            }
            ServerMessage::Draining => {
                info!("server is draining; finishing in-flight work then disconnecting");
                self.draining = true;
            }
            ServerMessage::Error { code, message } => {
                error!(code, %message, "protocol error from server");
            }
            ServerMessage::InitAck { .. } | ServerMessage::Reject { .. } => {
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
            ServerMessage::KnownDerivations { job_id, known } => {
                self.on_known_derivations(job_id, known);
            }
            ServerMessage::EvalCachePullResult { job_id, outcome } => {
                self.eval_cache_recv.deliver_pull_result(&job_id, outcome);
            }
            ServerMessage::EvalCacheChunk {
                job_id,
                data,
                offset,
                is_final,
            } => {
                self.eval_cache_recv
                    .deliver_pull_chunk(&job_id, data, offset, is_final);
            }
            ServerMessage::EvalCachePushGrant { job_id, mode } => {
                self.eval_cache_recv.deliver_push_grant(&job_id, mode);
            }
        }
        Ok(())
    }

    // ── Job completion / heartbeat ────────────────────────────────────────────

    /// Handle job completion from the `done_rx` channel: clean up per-job
    /// state, report the result to the server, and request a new job if the
    /// worker still has capacity.
    async fn on_job_done(&mut self, job_id: String, result: Result<()>) -> Result<()> {
        self.jobs.abort_senders.remove(&job_id);
        crate::proto::job::forget_cache_waiters_for_job(&self.cache_waiters, &job_id);
        self.known_derivation_waiters
            .lock()
            .unwrap()
            .remove(&job_id);
        self.nar_recv.forget_job(&job_id);
        self.eval_cache_recv.forget_job(&job_id);
        self.credentials.clear();

        let completed_kind = self.jobs.job_kinds.remove(&job_id);

        match result {
            Ok(()) => {
                info!(%job_id, "job completed");
                self.writer
                    .send(ClientMessage::JobCompleted { job_id })
                    .await?;
            }
            Err(e) => {
                let error_chain = format!("{e:#}");
                let (kind, missing_paths) = crate::executor::failure::wire_failure(&e);
                error!(%job_id, error = %error_chain, ?kind, "job failed");
                self.writer
                    .send(ClientMessage::JobFailed {
                        job_id,
                        error: error_chain,
                        kind,
                        missing_paths,
                    })
                    .await?;
            }
        }

        if !self.draining {
            let kind = completed_kind.unwrap_or(JobKind::Build);
            if self.jobs.active(kind.clone()) < self.max_for(kind.clone()) {
                self.writer.send(ClientMessage::RequestJob { kind }).await?;
            }
        }

        Ok(())
    }

    /// Heartbeat tick: send live host metrics and request more jobs if the
    /// worker has capacity.
    async fn on_heartbeat(&mut self) {
        send_live_metrics(&self.writer);
        if self.draining {
            return;
        }
        let active_eval = self.jobs.active(JobKind::Flake);
        let active_build = self.jobs.active(JobKind::Build);
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
        if want_eval
            && let Err(e) = self
                .writer
                .send(ClientMessage::RequestJob {
                    kind: JobKind::Flake,
                })
                .await
        {
            warn!(error = %e, "heartbeat RequestJob Flake send failed");
        }
        if want_build
            && let Err(e) = self
                .writer
                .send(ClientMessage::RequestJob {
                    kind: JobKind::Build,
                })
                .await
        {
            warn!(error = %e, "heartbeat RequestJob Build send failed");
        }
    }

    // ── Job list / scoring ────────────────────────────────────────────────────

    fn on_job_list_chunk(&mut self, cands: Vec<JobCandidate>, is_final: bool) {
        debug!(count = cands.len(), is_final, "received job list chunk");
        if self.draining {
            return;
        }
        {
            let mut g = self.candidates.lock().unwrap();
            for c in &cands {
                g.insert(c.job_id.clone(), c.clone());
            }
        }
        spawn_scoring_task(
            self.scorer,
            Arc::clone(&self.executor.store),
            Arc::clone(&self.last_scores),
            self.writer.clone(),
            cands,
            false,
            is_final,
            Vec::new(),
        );
    }

    fn on_job_offer(&mut self, cands: Vec<JobCandidate>) {
        debug!(count = cands.len(), "received job offer");
        if self.draining {
            return;
        }
        let new_candidates: Vec<JobCandidate> = {
            let mut g = self.candidates.lock().unwrap();
            cands
                .into_iter()
                .filter(|c| {
                    let changed = g.get(&c.job_id) != Some(c);
                    if changed {
                        g.insert(c.job_id.clone(), c.clone());
                    }
                    changed
                })
                .collect()
        };
        if !new_candidates.is_empty() {
            let mut request_after = Vec::new();
            if self.jobs.active(JobKind::Build) < self.max_build {
                request_after.push(JobKind::Build);
            }
            if self.jobs.active(JobKind::Flake) < self.max_eval {
                request_after.push(JobKind::Flake);
            }

            spawn_scoring_task(
                self.scorer,
                Arc::clone(&self.executor.store),
                Arc::clone(&self.last_scores),
                self.writer.clone(),
                new_candidates,
                true,
                true,
                request_after,
            );
        }
    }

    fn on_revoke_job(&mut self, job_ids: Vec<String>) {
        debug!(?job_ids, "jobs revoked");
        let mut cands = self.candidates.lock().unwrap();
        let mut scores = self.last_scores.lock().unwrap();
        for id in &job_ids {
            cands.remove(id);
            scores.remove(id);
        }
    }

    async fn on_request_all_scores(&mut self) {
        let all: Vec<JobCandidate> = self.candidates.lock().unwrap().values().cloned().collect();
        debug!(
            count = all.len(),
            "RequestAllScores - re-scoring all cached candidates"
        );
        if all.is_empty() {
            if let Err(e) = send_score_chunks(&self.writer, vec![]).await {
                warn!(error = %e, "send_score_chunks (empty) failed");
            }
        } else {
            spawn_scoring_task(
                self.scorer,
                Arc::clone(&self.executor.store),
                Arc::clone(&self.last_scores),
                self.writer.clone(),
                all,
                true,
                true,
                Vec::new(),
            );
        }
    }

    // ── Job lifecycle ─────────────────────────────────────────────────────────

    /// Drop a job from the local candidate + score caches so a later server
    /// re-offer is treated as new and re-scored (the delta filter skips
    /// unchanged cached entries).
    fn forget_candidate(&self, job_id: &str) {
        self.candidates.lock().unwrap().remove(job_id);
        self.last_scores.lock().unwrap().remove(job_id);
    }

    async fn on_assign_job(&mut self, job_id: String, job: Job) -> Result<()> {
        // Drop the cached candidate + score on any reject too (not just accept):
        // the server re-queues a rejected job and re-offers it, but our delta
        // filter would skip an unchanged cached entry, so it would never be
        // re-scored and would sit unassigned despite free capacity.
        if self.draining {
            warn!(%job_id, "rejecting assigned job - draining");
            self.forget_candidate(&job_id);
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
        let active_count = self.jobs.active(kind.clone());
        let max = self.max_for(kind.clone());

        if active_count >= max {
            warn!(%job_id, ?kind, active = active_count, limit = max, "rejecting assigned job - at capacity");
            self.forget_candidate(&job_id);
            self.writer
                .send(ClientMessage::AssignJobResponse {
                    job_id,
                    accepted: false,
                    reason: Some(format!("at capacity ({}/{})", active_count, max)),
                })
                .await?;
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

        self.jobs.job_kinds.insert(job_id.clone(), kind.clone());
        self.forget_candidate(&job_id);

        let (abort_tx, abort_rx) = watch::channel(false);
        self.jobs.abort_senders.insert(job_id.clone(), abort_tx);

        let executor = self.executor.clone();
        let job_store = Arc::clone(&executor.store);
        let credentials = self.credentials.clone();
        let job_writer = self.writer.clone();
        let job_cache_waiters = Arc::clone(&self.cache_waiters);
        let job_known_derivation_waiters = Arc::clone(&self.known_derivation_waiters);
        let job_nar_recv = self.nar_recv.clone();
        let job_eval_cache_recv = self.eval_cache_recv.clone();
        let job_done_tx = self.jobs.done_tx.clone();
        let jid = job_id.clone();

        tokio::spawn(async move {
            let mut updater = JobUpdater::new(
                jid.clone(),
                job_writer,
                job_cache_waiters,
                job_known_derivation_waiters,
                job_nar_recv,
                job_eval_cache_recv,
                Some(job_store),
            );
            let result = run_job(executor, job, &mut updater, &credentials, abort_rx).await;
            let _ = job_done_tx.send((jid, result));
        });

        if active_count + 1 < max {
            self.writer.send(ClientMessage::RequestJob { kind }).await?;
        }

        Ok(())
    }

    fn on_abort_job(&mut self, job_id: String, reason: String) {
        warn!(%job_id, %reason, "job aborted by server");
        if let Some(tx) = self.jobs.abort_senders.get(&job_id) {
            let _ = tx.send(true);
        }
    }

    // ── Credentials ───────────────────────────────────────────────────────────

    fn on_credential(&mut self, kind: gradient_proto::messages::CredentialKind, data: Vec<u8>) {
        debug!(?kind, "received credential");
        self.credentials.store(kind, data);
    }

    // ── NAR transfer ──────────────────────────────────────────────────────────

    async fn on_nar_push(
        &mut self,
        job_id: String,
        store_path: String,
        data: Vec<u8>,
        offset: u64,
        is_final: bool,
    ) {
        debug!(%job_id, %store_path, offset, is_final, bytes = data.len(), "received NAR chunk from server");
        self.nar_recv
            .accept_chunk(&job_id, &store_path, data, offset, is_final)
            .await;
    }

    fn on_cache_status(&mut self, query_id: String, cached: Vec<CachedPath>) {
        let count = cached.len();
        if !crate::proto::job::deliver_cache_reply(&self.cache_waiters, &query_id, Ok(cached)) {
            debug!(%query_id, count, "CacheStatus arrived after waiter cleared");
        }
    }

    fn on_cache_error(&mut self, query_id: String, message: String) {
        if !crate::proto::job::deliver_cache_reply(&self.cache_waiters, &query_id, Err(message)) {
            debug!(%query_id, "CacheError arrived after waiter cleared");
        }
    }

    fn on_known_derivations(&mut self, job_id: String, known: Vec<String>) {
        if let Some(tx) = self
            .known_derivation_waiters
            .lock()
            .unwrap()
            .remove(&job_id)
        {
            let _ = tx.send(known);
        } else {
            debug!(%job_id, count = known.len(), "KnownDerivations arrived after waiter cleared");
        }
    }

    // ── Auth ──────────────────────────────────────────────────────────────────

    async fn on_auth_challenge(&mut self, peers: Vec<String>) -> Result<()> {
        debug!(
            ?peers,
            "mid-connection AuthChallenge - sending AuthResponse"
        );
        let peer_tokens = self.config.peer_tokens();
        let tokens = WorkerConfig::resolve_tokens_for_challenge(&peer_tokens, &peers);
        self.writer
            .send(ClientMessage::AuthResponse { tokens })
            .await?;
        Ok(())
    }

    fn on_auth_update(
        &mut self,
        authorized_peers: Vec<String>,
        failed_peers: Vec<gradient_proto::messages::FailedPeer>,
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

/// Sample live host load off the dispatch thread (the CPU sample blocks for
/// [`sysinfo::MINIMUM_CPU_UPDATE_INTERVAL`]) and send it to the scheduler.
/// `disk_speed_mbps` / `network_speed_mbps` come from passive EWMA
/// accumulators and stay `None` until the first build / NAR transfer. Detached
/// (not awaited by the heartbeat tick): the blocking sample runs on
/// `spawn_blocking`, then the async send runs on the runtime once it finishes.
fn send_live_metrics(writer: &ProtoWriter) {
    let writer = writer.clone();
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
                disk_speed_mbps: crate::metrics::throughput::DISK.current(),
                network_speed_mbps: crate::metrics::throughput::NETWORK.current(),
            })
            .await
        {
            debug!(error = %e, "heartbeat WorkerMetrics send failed");
        }
    });
}

// ── Job runner ────────────────────────────────────────────────────────────────

async fn run_job(
    executor: JobExecutor,
    job: Job,
    updater: &mut JobUpdater,
    credentials: &CredentialStore,
    abort: watch::Receiver<bool>,
) -> Result<()> {
    match job {
        Job::Flake(flake_job) => {
            executor
                .execute_flake_job(flake_job, updater, credentials, abort)
                .await
        }
        Job::Build(build_job) => {
            executor
                .execute_build_job(build_job, updater, credentials, abort)
                .await
        }
    }
}
