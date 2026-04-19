/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Worker dispatch loop: drives `tokio::select!` over the server connection,
//! job completion, and heartbeats.
//!
//! [`run_dispatch_loop`] is the main entry point.  Each inbound
//! [`ServerMessage`] is routed to a method on [`MessageHandler`], which holds
//! all per-connection context.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use proto::messages::{CachedPath, ClientMessage, Job, JobCandidate, JobKind, ServerMessage};
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

use crate::config::WorkerConfig;
use crate::connection::{ProtoConnection, ProtoWriter};
use crate::executor::JobExecutor;
use crate::proto::credentials::CredentialStore;
use crate::proto::job::{CacheWaiters, JobUpdater, KnownDerivationWaiters};
use crate::proto::scorer::JobScorer;

use super::scoring::{send_score_chunks, spawn_scoring_task};

// ── Dispatch loop ─────────────────────────────────────────────────────────────

/// Returns the draining flag: `true` if the server sent `Draining` before
/// closing the connection.
pub(super) async fn run_dispatch_loop(
    conn: ProtoConnection,
    config: &WorkerConfig,
    executor: &JobExecutor,
    scorer: JobScorer,
    credentials: &mut CredentialStore,
    candidates: &Arc<Mutex<HashMap<String, JobCandidate>>>,
    last_scores: &Arc<Mutex<HashMap<String, proto::messages::CandidateScore>>>,
) -> Result<bool> {
    let (writer, mut reader) = conn.split();

    let cache_waiters: CacheWaiters = Arc::new(Mutex::new(HashMap::new()));
    let known_derivation_waiters: KnownDerivationWaiters = Arc::new(Mutex::new(HashMap::new()));
    let mut draining = false;
    let nar_recv = crate::proto::nar_recv::NarReceiver::new();
    let mut abort_senders: HashMap<String, watch::Sender<bool>> = HashMap::new();
    let mut job_kinds: HashMap<String, JobKind> = HashMap::new();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<(String, Result<()>)>();

    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(10));
    heartbeat.tick().await;

    let max_eval = config.max_concurrent_evaluations;
    let max_build = config.max_concurrent_builds;

    info!("entering dispatch loop");

    loop {
        tokio::select! {
            biased;

            Some((job_id, result)) = done_rx.recv() => {
                on_job_done(
                    job_id, result,
                    &writer, &cache_waiters, &known_derivation_waiters, &nar_recv,
                    credentials, &mut job_kinds, &mut abort_senders,
                    &draining, max_eval, max_build,
                )?;
            }

            _ = heartbeat.tick() => {
                on_heartbeat(&writer, &job_kinds, &draining, max_eval, max_build);
            }

            msg_result = reader.recv() => {
                let msg = match msg_result? {
                    Some(m) => m,
                    None => {
                        info!("server closed connection");
                        break;
                    }
                };
                let started = std::time::Instant::now();
                let kind = msg_kind(&msg);
                let result = MessageHandler {
                    writer: &writer,
                    cache_waiters: &cache_waiters,
                    known_derivation_waiters: &known_derivation_waiters,
                    nar_recv: &nar_recv,
                    abort_senders: &mut abort_senders,
                    job_kinds: &mut job_kinds,
                    max_eval,
                    max_build,
                    done_tx: &done_tx,
                    credentials,
                    candidates,
                    last_scores,
                    scorer,
                    executor,
                    config,
                    draining: &mut draining,
                }
                .dispatch(msg)
                .await;
                let elapsed_ms = started.elapsed().as_millis();
                if elapsed_ms > 1_000 {
                    warn!(kind, elapsed_ms, "slow message dispatch — loop was blocked");
                }
                result?;
            }
        }
    }

    Ok(draining)
}

// ── Select arm helpers ────────────────────────────────────────────────────────

/// Handle job completion from the `done_rx` channel.
///
/// Cleans up per-job state, reports the result to the server, and requests
/// a new job if the worker still has capacity.
#[allow(clippy::too_many_arguments)]
fn on_job_done(
    job_id: String,
    result: Result<()>,
    writer: &ProtoWriter,
    cache_waiters: &CacheWaiters,
    known_derivation_waiters: &KnownDerivationWaiters,
    nar_recv: &crate::proto::nar_recv::NarReceiver,
    credentials: &mut CredentialStore,
    job_kinds: &mut HashMap<String, JobKind>,
    abort_senders: &mut HashMap<String, watch::Sender<bool>>,
    draining: &bool,
    max_eval: u32,
    max_build: u32,
) -> Result<()> {
    abort_senders.remove(&job_id);
    cache_waiters.lock().unwrap().remove(&job_id);
    known_derivation_waiters.lock().unwrap().remove(&job_id);
    nar_recv.forget_job(&job_id);
    credentials.clear();

    let completed_kind = job_kinds.remove(&job_id);

    match result {
        Ok(()) => {
            info!(%job_id, "job completed");
            writer.send(ClientMessage::JobCompleted { job_id })?;
        }
        Err(e) => {
            error!(%job_id, error = %e, "job failed");
            writer.send(ClientMessage::JobFailed {
                job_id,
                error: e.to_string(),
            })?;
        }
    }

    if !draining {
        let kind = completed_kind.unwrap_or(JobKind::Build);
        let active = job_kinds.values().filter(|k| **k == kind).count() as u32;
        let max = match &kind {
            JobKind::Flake => max_eval,
            JobKind::Build => max_build,
        };
        if active < max {
            writer.send(ClientMessage::RequestJob { kind })?;
        }
    }

    Ok(())
}

/// Heartbeat tick: request more jobs if the worker has capacity.
fn on_heartbeat(
    writer: &ProtoWriter,
    job_kinds: &HashMap<String, JobKind>,
    draining: &bool,
    max_eval: u32,
    max_build: u32,
) {
    if *draining {
        return;
    }
    let active_eval = job_kinds.values().filter(|k| **k == JobKind::Flake).count() as u32;
    let active_build = job_kinds.values().filter(|k| **k == JobKind::Build).count() as u32;
    let want_eval = active_eval < max_eval;
    let want_build = active_build < max_build;
    debug!(
        active_eval,
        max_eval,
        active_build,
        max_build,
        request_eval = want_eval,
        request_build = want_build,
        "heartbeat tick"
    );
    if want_eval
        && let Err(e) = writer.send(ClientMessage::RequestJob {
            kind: JobKind::Flake,
        }) {
            warn!(error = %e, "heartbeat RequestJob Flake send failed");
        }
    if want_build
        && let Err(e) = writer.send(ClientMessage::RequestJob {
            kind: JobKind::Build,
        }) {
            warn!(error = %e, "heartbeat RequestJob Build send failed");
        }
}

// ── Message handler ───────────────────────────────────────────────────────────

/// Holds the per-connection context needed to process a single `ServerMessage`.
///
/// Constructed fresh for each message in the dispatch loop so the lifetime
/// scope is tight.  All fields are borrows — no ownership transfer.
pub(super) struct MessageHandler<'a> {
    pub writer: &'a ProtoWriter,
    pub cache_waiters: &'a CacheWaiters,
    pub known_derivation_waiters: &'a KnownDerivationWaiters,
    pub nar_recv: &'a crate::proto::nar_recv::NarReceiver,
    pub abort_senders: &'a mut HashMap<String, watch::Sender<bool>>,
    pub job_kinds: &'a mut HashMap<String, JobKind>,
    pub max_eval: u32,
    pub max_build: u32,
    pub done_tx: &'a mpsc::UnboundedSender<(String, Result<()>)>,
    pub credentials: &'a mut CredentialStore,
    pub candidates: &'a Arc<Mutex<HashMap<String, JobCandidate>>>,
    pub last_scores: &'a Arc<Mutex<HashMap<String, proto::messages::CandidateScore>>>,
    pub scorer: JobScorer,
    pub executor: &'a JobExecutor,
    pub config: &'a WorkerConfig,
    pub draining: &'a mut bool,
}

impl<'a> MessageHandler<'a> {
    /// Route `msg` to the appropriate handler method.
    pub async fn dispatch(self, msg: ServerMessage) -> Result<()> {
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
            ServerMessage::AssignJob {
                job_id,
                job,
                timeout_secs: _,
            } => {
                self.on_assign_job(job_id, job)?;
            }
            ServerMessage::AbortJob { job_id, reason } => {
                self.on_abort_job(job_id, reason);
            }
            ServerMessage::Credential { kind, data } => {
                self.on_credential(kind, data);
            }
            ServerMessage::NarPush {
                job_id,
                store_path,
                data,
                offset,
                is_final,
            } => {
                self.on_nar_push(job_id, store_path, data, offset, is_final);
            }
            ServerMessage::PresignedDownload {
                job_id,
                store_path,
                url: _,
            } => {
                debug!(%job_id, %store_path, "received presigned download URL");
            }
            ServerMessage::PresignedUpload {
                job_id,
                store_path,
                url,
                method,
                headers,
            } => {
                self.on_presigned_upload(job_id, store_path, url, method, headers)
                    .await;
            }
            ServerMessage::RequestAllScores => {
                self.on_request_all_scores();
            }
            ServerMessage::Draining => {
                info!("server is draining; finishing in-flight work then disconnecting");
                *self.draining = true;
            }
            ServerMessage::Error { code, message } => {
                error!(code, %message, "protocol error from server");
            }
            ServerMessage::InitAck { .. } | ServerMessage::Reject { .. } => {
                warn!("unexpected handshake message in dispatch loop — ignoring");
            }
            ServerMessage::AuthChallenge { peers } => {
                self.on_auth_challenge(peers)?;
            }
            ServerMessage::AuthUpdate {
                authorized_peers,
                failed_peers,
            } => {
                self.on_auth_update(authorized_peers, failed_peers);
            }
            ServerMessage::CacheStatus { job_id, cached } => {
                self.on_cache_status(job_id, cached);
            }
            ServerMessage::KnownDerivations { job_id, known } => {
                self.on_known_derivations(job_id, known);
            }
        }
        Ok(())
    }

    // ── Job list / scoring ────────────────────────────────────────────────────

    fn on_job_list_chunk(self, cands: Vec<JobCandidate>, is_final: bool) {
        debug!(count = cands.len(), is_final, "received job list chunk");
        if *self.draining {
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
            Arc::clone(self.last_scores),
            self.writer.clone(),
            cands,
            false,
            is_final,
        );
    }

    fn on_job_offer(self, cands: Vec<JobCandidate>) {
        debug!(count = cands.len(), "received job offer");
        if *self.draining {
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
            spawn_scoring_task(
                self.scorer,
                Arc::clone(&self.executor.store),
                Arc::clone(self.last_scores),
                self.writer.clone(),
                new_candidates,
                true,
                true,
            );
        }
    }

    fn on_revoke_job(self, job_ids: Vec<String>) {
        debug!(?job_ids, "jobs revoked");
        let mut cands = self.candidates.lock().unwrap();
        let mut scores = self.last_scores.lock().unwrap();
        for id in &job_ids {
            cands.remove(id);
            scores.remove(id);
        }
    }

    fn on_request_all_scores(self) {
        let all: Vec<JobCandidate> = self.candidates.lock().unwrap().values().cloned().collect();
        debug!(
            count = all.len(),
            "RequestAllScores — re-scoring all cached candidates"
        );
        if all.is_empty() {
            if let Err(e) = send_score_chunks(self.writer, vec![]) {
                warn!(error = %e, "send_score_chunks (empty) failed");
            }
        } else {
            spawn_scoring_task(
                self.scorer,
                Arc::clone(&self.executor.store),
                Arc::clone(self.last_scores),
                self.writer.clone(),
                all,
                true,
                true,
            );
        }
    }

    // ── Job lifecycle ─────────────────────────────────────────────────────────

    fn on_assign_job(self, job_id: String, job: Job) -> Result<()> {
        if *self.draining {
            warn!(%job_id, "rejecting assigned job — draining");
            self.writer.send(ClientMessage::AssignJobResponse {
                job_id,
                accepted: false,
                reason: Some("worker is draining".to_owned()),
            })?;
            return Ok(());
        }

        let kind = match &job {
            Job::Flake(_) => JobKind::Flake,
            Job::Build(_) => JobKind::Build,
        };
        let active_count = self.job_kinds.values().filter(|k| **k == kind).count() as u32;
        let max = match &kind {
            JobKind::Flake => self.max_eval,
            JobKind::Build => self.max_build,
        };

        if active_count >= max {
            warn!(%job_id, ?kind, active = active_count, limit = max, "rejecting assigned job — at capacity");
            self.writer.send(ClientMessage::AssignJobResponse {
                job_id,
                accepted: false,
                reason: Some(format!("at capacity ({}/{})", active_count, max)),
            })?;
            return Ok(());
        }

        info!(%job_id, ?kind, "job assigned — accepting");
        self.writer.send(ClientMessage::AssignJobResponse {
            job_id: job_id.clone(),
            accepted: true,
            reason: None,
        })?;

        self.job_kinds.insert(job_id.clone(), kind.clone());
        self.candidates.lock().unwrap().remove(&job_id);
        self.last_scores.lock().unwrap().remove(&job_id);

        let (abort_tx, abort_rx) = watch::channel(false);
        self.abort_senders.insert(job_id.clone(), abort_tx);

        let executor = self.executor.clone();
        let credentials = self.credentials.clone();
        let job_writer = self.writer.clone();
        let job_cache_waiters = Arc::clone(self.cache_waiters);
        let job_known_derivation_waiters = Arc::clone(self.known_derivation_waiters);
        let job_nar_recv = self.nar_recv.clone();
        let job_done_tx = self.done_tx.clone();
        let jid = job_id.clone();

        tokio::spawn(async move {
            let mut updater = JobUpdater::new(
                jid.clone(),
                job_writer,
                job_cache_waiters,
                job_known_derivation_waiters,
                job_nar_recv,
            );
            let result = run_job(executor, job, &mut updater, &credentials, abort_rx).await;
            let _ = job_done_tx.send((jid, result));
        });

        if active_count + 1 < max {
            self.writer.send(ClientMessage::RequestJob { kind })?;
        }

        Ok(())
    }

    fn on_abort_job(self, job_id: String, reason: String) {
        warn!(%job_id, %reason, "job aborted by server");
        if let Some(tx) = self.abort_senders.get(&job_id) {
            let _ = tx.send(true);
        }
    }

    // ── Credentials ───────────────────────────────────────────────────────────

    fn on_credential(self, kind: proto::messages::CredentialKind, data: Vec<u8>) {
        debug!(?kind, "received credential");
        self.credentials.store(kind, data);
    }

    // ── NAR transfer ──────────────────────────────────────────────────────────

    fn on_nar_push(
        self,
        job_id: String,
        store_path: String,
        data: Vec<u8>,
        offset: u64,
        is_final: bool,
    ) {
        debug!(%job_id, %store_path, offset, is_final, bytes = data.len(), "received NAR chunk from server");
        self.nar_recv
            .accept_chunk(&job_id, &store_path, data, is_final);
    }

    async fn on_presigned_upload(
        self,
        job_id: String,
        store_path: String,
        url: String,
        method: String,
        headers: Vec<(String, String)>,
    ) {
        debug!(%job_id, %store_path, %method, "received presigned upload URL");
        let store = std::sync::Arc::clone(&self.executor.store);
        let signing_key = self.credentials.signing_key();
        let signing_key_str = signing_key.as_ref().map(|k| k.expose().to_owned());
        if let Err(e) = crate::proto::nar::upload_presigned(
            &job_id,
            &store_path,
            &url,
            &method,
            &headers,
            self.writer,
            Some(&store),
            signing_key_str.as_deref(),
        )
        .await
        {
            error!(%job_id, %store_path, error = %e, "presigned NAR upload failed");
        }
    }

    fn on_cache_status(self, job_id: String, cached: Vec<CachedPath>) {
        if let Some(tx) = self.cache_waiters.lock().unwrap().remove(&job_id) {
            let _ = tx.send(cached);
        } else {
            debug!(%job_id, count = cached.len(), "CacheStatus arrived after waiter cleared");
        }
    }

    fn on_known_derivations(self, job_id: String, known: Vec<String>) {
        if let Some(tx) = self.known_derivation_waiters.lock().unwrap().remove(&job_id) {
            let _ = tx.send(known);
        } else {
            debug!(%job_id, count = known.len(), "KnownDerivations arrived after waiter cleared");
        }
    }

    // ── Auth ──────────────────────────────────────────────────────────────────

    fn on_auth_challenge(self, peers: Vec<String>) -> Result<()> {
        debug!(
            ?peers,
            "mid-connection AuthChallenge — sending AuthResponse"
        );
        let peer_tokens = self.config.peer_tokens();
        let tokens = WorkerConfig::resolve_tokens_for_challenge(&peer_tokens, &peers);
        self.writer.send(ClientMessage::AuthResponse { tokens })?;
        Ok(())
    }

    fn on_auth_update(
        self,
        authorized_peers: Vec<String>,
        failed_peers: Vec<proto::messages::FailedPeer>,
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

// ── Job runner ────────────────────────────────────────────────────────────────

pub(super) fn msg_kind(msg: &ServerMessage) -> &'static str {
    match msg {
        ServerMessage::JobListChunk { .. } => "JobListChunk",
        ServerMessage::JobOffer { .. } => "JobOffer",
        ServerMessage::RevokeJob { .. } => "RevokeJob",
        ServerMessage::AssignJob { .. } => "AssignJob",
        ServerMessage::AbortJob { .. } => "AbortJob",
        ServerMessage::Credential { .. } => "Credential",
        ServerMessage::NarPush { .. } => "NarPush",
        ServerMessage::PresignedDownload { .. } => "PresignedDownload",
        ServerMessage::PresignedUpload { .. } => "PresignedUpload",
        ServerMessage::RequestAllScores => "RequestAllScores",
        ServerMessage::Draining => "Draining",
        ServerMessage::Error { .. } => "Error",
        ServerMessage::InitAck { .. } => "InitAck",
        ServerMessage::Reject { .. } => "Reject",
        ServerMessage::AuthChallenge { .. } => "AuthChallenge",
        ServerMessage::AuthUpdate { .. } => "AuthUpdate",
        ServerMessage::CacheStatus { .. } => "CacheStatus",
        ServerMessage::KnownDerivations { .. } => "KnownDerivations",
    }
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
            executor
                .execute_flake_job(flake_job, updater, credentials, abort)
                .await
        }
        Job::Build(build_job) => {
            executor
                .execute_build_job(build_job, updater, credentials)
                .await
        }
    }
}
