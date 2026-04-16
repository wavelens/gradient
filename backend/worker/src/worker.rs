/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Top-level worker runtime.
//!
//! [`Worker`] connects to the Gradient server, performs the handshake,
//! registers capabilities, and then drives the job dispatch loop.
//!
//! The `run()` method splits the connection so the recv loop stays live
//! while jobs execute in separate tasks.  [`ServerMessage::AbortJob`] fires
//! a `watch` channel that the executor checks between subprocess steps.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use proto::messages::{CachedPath, ClientMessage, Job, JobCandidate, JobKind, ServerMessage};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, error, info, warn};

use crate::config::WorkerConfig;
use crate::connection::ProtoConnection;
use crate::connection::handshake::perform_handshake;
use crate::executor::{JobExecutor, WorkerEvaluator};
use crate::nix::store::LocalNixStore;
use crate::proto::credentials::CredentialStore;
use crate::proto::job::JobUpdater;
use crate::proto::scorer::JobScorer;

/// The running worker instance.
pub struct Worker {
    config: WorkerConfig,
    /// `None` while `run()` is active (it takes ownership of the connection).
    conn: Option<ProtoConnection>,
    executor: JobExecutor,
    scorer: JobScorer,
    /// Shared across clones so `Credential` messages update the running job.
    credentials: CredentialStore,
    /// Set to `true` after the server sends `Draining`.
    draining: bool,
    /// Local cache of job candidates received from the server.
    /// Updated on `JobListChunk` and `JobOffer`; entries removed on `RevokeJob`.
    /// Used to re-score and respond to `RequestAllScores` after a server restart.
    candidates: HashMap<String, proto::messages::JobCandidate>,
    /// Last known score per candidate (keyed by job_id).
    /// Used to filter `RequestJobChunk` to only changed scores.
    last_scores: HashMap<String, proto::messages::CandidateScore>,
}

impl Worker {
    /// Connect to the server at `config.server_url`, complete the handshake,
    /// and advertise build capabilities.
    pub async fn connect(config: WorkerConfig) -> Result<Self> {
        let mut conn = ProtoConnection::open(&config.server_url).await?;

        let peer_id = load_or_generate_id(&config.data_dir, config.worker_id.as_deref())
            .context("failed to load or generate persistent worker ID")?;
        let peer_tokens = config.peer_tokens();

        let handshake =
            perform_handshake(&mut conn, peer_id, peer_tokens, config.capabilities()).await?;

        conn.set_server_version(handshake.server_version);
        info!(
            negotiated = ?handshake.negotiated,
            server_version = conn.server_version(),
            "capabilities negotiated"
        );

        if handshake.negotiated.build {
            let architectures = config
                .architectures
                .clone()
                .unwrap_or_else(|| vec![crate::config::host_system()]);
            let system_features = config.system_features.clone().unwrap_or_default();
            info!(
                ?architectures,
                ?system_features,
                max_concurrent_builds = config.max_concurrent_builds,
                "advertising build capabilities"
            );
            conn.send(ClientMessage::WorkerCapabilities {
                architectures,
                system_features,
                max_concurrent_builds: config.max_concurrent_builds,
            })
            .await?;
        }

        conn.send(ClientMessage::RequestJobList).await?;
        // Signal initial capacity — one per kind; re-sends after each AssignJob fill remaining slots.
        conn.send(ClientMessage::RequestJob { kind: JobKind::Flake }).await?;
        conn.send(ClientMessage::RequestJob { kind: JobKind::Build }).await?;

        let store = LocalNixStore::connect().await?;
        let evaluator = WorkerEvaluator::new(config.eval_workers, config.max_evals_per_worker);
        let executor = JobExecutor::new(
            store,
            evaluator,
            config.binpath_nix.clone(),
            config.binpath_ssh.clone(),
        );

        Ok(Self {
            config,
            conn: Some(conn),
            executor,
            scorer: JobScorer::new(),
            credentials: CredentialStore::new(),
            draining: false,
            candidates: HashMap::new(),
            last_scores: HashMap::new(),
        })
    }

    /// Accept an incoming server-initiated WebSocket connection.
    pub async fn from_accepted(
        ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        config: WorkerConfig,
    ) -> Result<Self> {
        let mut conn = ProtoConnection::from_accepted(ws);

        let peer_id = load_or_generate_id(&config.data_dir, config.worker_id.as_deref())
            .context("failed to load or generate persistent worker ID")?;
        let peer_tokens = config.peer_tokens();

        let handshake =
            perform_handshake(&mut conn, peer_id, peer_tokens, config.capabilities()).await?;
        conn.set_server_version(handshake.server_version);
        info!(
            negotiated = ?handshake.negotiated,
            server_version = conn.server_version(),
            "incoming connection: capabilities negotiated"
        );

        if handshake.negotiated.build {
            let architectures = config
                .architectures
                .clone()
                .unwrap_or_else(|| vec![crate::config::host_system()]);
            let system_features = config.system_features.clone().unwrap_or_default();
            info!(
                ?architectures,
                ?system_features,
                max_concurrent_builds = config.max_concurrent_builds,
                "advertising build capabilities"
            );
            conn.send(ClientMessage::WorkerCapabilities {
                architectures,
                system_features,
                max_concurrent_builds: config.max_concurrent_builds,
            })
            .await?;
        }

        conn.send(ClientMessage::RequestJobList).await?;
        // Signal initial capacity — one per kind; re-sends after each AssignJob fill remaining slots.
        conn.send(ClientMessage::RequestJob { kind: JobKind::Flake }).await?;
        conn.send(ClientMessage::RequestJob { kind: JobKind::Build }).await?;

        let store = LocalNixStore::connect().await?;
        let evaluator = WorkerEvaluator::new(config.eval_workers, config.max_evals_per_worker);
        let executor = JobExecutor::new(
            store,
            evaluator,
            config.binpath_nix.clone(),
            config.binpath_ssh.clone(),
        );

        Ok(Self {
            config,
            conn: Some(conn),
            executor,
            scorer: JobScorer::new(),
            credentials: CredentialStore::new(),
            draining: false,
            candidates: HashMap::new(),
            last_scores: HashMap::new(),
        })
    }

    /// Reconnect to the server: close any existing connection, open a fresh one,
    /// perform the handshake, and store it for the next `run()` call.
    pub async fn reconnect(&mut self) -> Result<()> {
        self.draining = false;

        let mut conn = ProtoConnection::open(&self.config.server_url).await?;

        let peer_id = load_or_generate_id(&self.config.data_dir, self.config.worker_id.as_deref())
            .context("failed to load persistent worker ID")?;
        let peer_tokens = self.config.peer_tokens();

        let handshake = perform_handshake(
            &mut conn,
            peer_id,
            peer_tokens,
            self.config.capabilities(),
        )
        .await?;

        conn.set_server_version(handshake.server_version);
        info!(
            negotiated = ?handshake.negotiated,
            server_version = conn.server_version(),
            "reconnected and re-negotiated capabilities"
        );

        if handshake.negotiated.build {
            conn.send(ClientMessage::WorkerCapabilities {
                architectures: vec![],
                system_features: vec![],
                max_concurrent_builds: self.config.max_concurrent_builds,
            })
            .await?;
        }

        conn.send(ClientMessage::RequestJobList).await?;
        // Signal initial capacity — one per kind; re-sends after each AssignJob fill remaining slots.
        conn.send(ClientMessage::RequestJob { kind: JobKind::Flake }).await?;
        conn.send(ClientMessage::RequestJob { kind: JobKind::Build }).await?;

        self.conn = Some(conn);
        Ok(())
    }

    /// Returns `true` if the server has asked this worker to drain.
    pub fn is_draining(&self) -> bool {
        self.draining
    }

    /// Main dispatch loop.
    ///
    /// Splits the connection so the recv loop stays live while jobs run in
    /// separate tasks.  Jobs are spawned with a `watch::Receiver<bool>` abort
    /// signal; `AbortJob` messages fire the corresponding sender.
    ///
    /// Returns `Ok(())` on a clean disconnect so the caller can reconnect.
    pub async fn run(&mut self) -> Result<()> {
        let conn = self.conn.take().context("run() called without a connection")?;
        let (writer, mut reader) = conn.split();

        // Shared: job tasks register a oneshot here when awaiting CacheStatus.
        let cache_waiters: Arc<Mutex<HashMap<String, oneshot::Sender<Vec<CachedPath>>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Shared: job tasks register here when awaiting NarPush chunks.
        let nar_recv = crate::proto::nar_recv::NarReceiver::new();

        // Abort senders: keyed by job_id; job tasks hold the receiver.
        let mut abort_senders: HashMap<String, watch::Sender<bool>> = HashMap::new();

        // Job kind tracking — needed to know which capacity counter to update
        // when a job completes and to re-send the right RequestJob kind.
        let mut job_kinds: HashMap<String, JobKind> = HashMap::new();

        // Channel used by job tasks to report completion back to the dispatch loop.
        let (done_tx, mut done_rx) =
            mpsc::unbounded_channel::<(String, Result<()>)>();

        // 10-second heartbeat: re-signal capacity to the server in case a
        // previous RequestJob was lost (e.g. server restart).
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(10));
        // Consume the first instant tick so the loop starts cleanly.
        heartbeat.tick().await;

        let max_eval = self.config.max_concurrent_evaluations;
        let max_build = self.config.max_concurrent_builds;

        info!("entering dispatch loop");

        loop {
            tokio::select! {
                biased;

                // A job task finished — send the result to the server.
                Some((job_id, result)) = done_rx.recv() => {
                    abort_senders.remove(&job_id);
                    cache_waiters.lock().unwrap().remove(&job_id);
                    nar_recv.forget_job(&job_id);
                    self.credentials.clear();

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

                    // A slot opened — chain one more request.
                    if !self.draining {
                        let kind = completed_kind.unwrap_or(JobKind::Build);
                        let active = job_kinds.values().filter(|k| **k == kind).count() as u32;
                        let max = match &kind { JobKind::Flake => max_eval, JobKind::Build => max_build };
                        if active < max {
                            writer.send(ClientMessage::RequestJob { kind })?;
                        }
                    }
                }

                // Heartbeat: re-signal capacity in case previous RequestJob
                // messages were lost (e.g. server restart).
                _ = heartbeat.tick() => {
                    if !self.draining {
                        let active_eval = job_kinds.values().filter(|k| **k == JobKind::Flake).count() as u32;
                        let active_build = job_kinds.values().filter(|k| **k == JobKind::Build).count() as u32;
                        let want_eval = active_eval < max_eval;
                        let want_build = active_build < max_build;
                        info!(
                            active_eval,
                            max_eval,
                            active_build,
                            max_build,
                            request_eval = want_eval,
                            request_build = want_build,
                            "heartbeat tick"
                        );
                        if want_eval
                            && let Err(e) = writer.send(ClientMessage::RequestJob { kind: JobKind::Flake })
                        {
                            warn!(error = %e, "heartbeat RequestJob Flake send failed");
                        }
                        if want_build
                            && let Err(e) = writer.send(ClientMessage::RequestJob { kind: JobKind::Build })
                        {
                            warn!(error = %e, "heartbeat RequestJob Build send failed");
                        }
                    }
                }

                // Receive next server message.
                msg_result = reader.recv() => {
                    let msg = match msg_result? {
                        Some(m) => m,
                        None => {
                            info!("server closed connection");
                            break;
                        }
                    };

                    self.handle_message(
                        msg,
                        &writer,
                        &cache_waiters,
                        &nar_recv,
                        &mut abort_senders,
                        &mut job_kinds,
                        max_eval,
                        max_build,
                        &done_tx,
                    ).await?;
                }
            }
        }

        Ok(())
    }

    /// Handle a single [`ServerMessage`] inside the dispatch loop.
    #[allow(clippy::too_many_arguments)]
    async fn handle_message(
        &mut self,
        msg: ServerMessage,
        writer: &crate::connection::ProtoWriter,
        cache_waiters: &Arc<Mutex<HashMap<String, oneshot::Sender<Vec<CachedPath>>>>>,
        nar_recv: &crate::proto::nar_recv::NarReceiver,
        abort_senders: &mut HashMap<String, watch::Sender<bool>>,
        job_kinds: &mut HashMap<String, JobKind>,
        max_eval: u32,
        max_build: u32,
        done_tx: &mpsc::UnboundedSender<(String, Result<()>)>,
    ) -> Result<()> {
        // Timing instrumentation: any single handler that takes more than a
        // second is starving the dispatch loop's `select!` (no heartbeat,
        // delayed `done_rx` / reader polling). Log it loudly so we can spot
        // the offender without having to rebuild with debug tracing.
        let started = std::time::Instant::now();
        let kind = msg_kind(&msg);
        let result = self
            .handle_message_inner(
                msg,
                writer,
                cache_waiters,
                nar_recv,
                abort_senders,
                job_kinds,
                max_eval,
                max_build,
                done_tx,
            )
            .await;
        let elapsed_ms = started.elapsed().as_millis();
        if elapsed_ms > 1_000 {
            warn!(
                kind,
                elapsed_ms,
                "slow handle_message — dispatch loop was blocked from polling heartbeat / done_rx for this duration"
            );
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_message_inner(
        &mut self,
        msg: ServerMessage,
        writer: &crate::connection::ProtoWriter,
        cache_waiters: &Arc<Mutex<HashMap<String, oneshot::Sender<Vec<CachedPath>>>>>,
        nar_recv: &crate::proto::nar_recv::NarReceiver,
        abort_senders: &mut HashMap<String, watch::Sender<bool>>,
        job_kinds: &mut HashMap<String, JobKind>,
        max_eval: u32,
        max_build: u32,
        done_tx: &mpsc::UnboundedSender<(String, Result<()>)>,
    ) -> Result<()> {
        match msg {
            ServerMessage::JobListChunk {
                candidates,
                is_final,
            } => {
                debug!(count = candidates.len(), is_final, "received job list chunk");
                if self.draining {
                    return Ok(());
                }
                // Cache the candidates for future re-scoring (e.g. RequestAllScores).
                for c in &candidates {
                    self.candidates.insert(c.job_id.clone(), c.clone());
                }
                let scores = self
                    .scorer
                    .score_candidates(&self.executor.store, &candidates)
                    .await?;
                // Record scores as baseline for future delta filtering.
                for s in &scores {
                    self.last_scores.insert(s.job_id.clone(), s.clone());
                }
                // Paginate at 1 000 scores per message; honour the upstream is_final.
                if is_final {
                    send_score_chunks(writer, scores)?;
                } else {
                    // Non-final chunk from a multi-chunk JobListChunk — accumulate.
                    // We'll send after is_final arrives; for now send each page
                    // as non-final.
                    for chunk in scores.chunks(1_000) {
                        writer.send(ClientMessage::RequestJobChunk {
                            scores: chunk.to_vec(),
                            is_final: false,
                        })?;
                    }
                }
            }

            ServerMessage::JobOffer { candidates } => {
                debug!(count = candidates.len(), "received job offer");
                if self.draining {
                    return Ok(());
                }
                // Only score candidates that are new or updated (not already cached
                // with the same content). The server sends delta-only JobOffers so
                // this is typically all of them, but guard against duplicates.
                let new_candidates: Vec<JobCandidate> = candidates
                    .into_iter()
                    .filter(|c| {
                        let already = self.candidates.get(&c.job_id);
                        already != Some(c)
                    })
                    .collect();
                for c in &new_candidates {
                    self.candidates.insert(c.job_id.clone(), c.clone());
                }
                if !new_candidates.is_empty() {
                    let scores = self
                        .scorer
                        .score_candidates(&self.executor.store, &new_candidates)
                        .await?;
                    // Only send scores that are new or changed vs. baseline.
                    let changed: Vec<_> = scores
                        .into_iter()
                        .filter(|s| self.last_scores.get(&s.job_id) != Some(s))
                        .collect();
                    for s in &changed {
                        self.last_scores.insert(s.job_id.clone(), s.clone());
                    }
                    send_score_chunks(writer, changed)?;
                }
            }

            ServerMessage::RevokeJob { job_ids } => {
                debug!(?job_ids, "jobs revoked");
                for id in &job_ids {
                    self.candidates.remove(id);
                    self.last_scores.remove(id);
                }
            }

            ServerMessage::AssignJob {
                job_id,
                job,
                timeout_secs: _,
            } => {
                if self.draining {
                    warn!(%job_id, "rejecting assigned job — draining");
                    writer.send(ClientMessage::AssignJobResponse {
                        job_id,
                        accepted: false,
                        reason: Some("worker is draining".to_owned()),
                    })?;
                    return Ok(());
                }

                // Determine job kind for capacity tracking.
                let kind = match &job {
                    Job::Flake(_) => JobKind::Flake,
                    Job::Build(_) => JobKind::Build,
                };

                // Enforce concurrency limits — reject if already at max.
                let active_count = job_kinds.values().filter(|k| **k == kind).count() as u32;
                let max = match &kind {
                    JobKind::Flake => max_eval,
                    JobKind::Build => max_build,
                };
                if active_count >= max {
                    warn!(%job_id, ?kind, active = active_count, limit = max, "rejecting assigned job — at capacity");
                    writer.send(ClientMessage::AssignJobResponse {
                        job_id,
                        accepted: false,
                        reason: Some(format!("at capacity ({}/{})", active_count, max)),
                    })?;
                    return Ok(());
                }

                info!(%job_id, ?kind, "job assigned — accepting");
                writer.send(ClientMessage::AssignJobResponse {
                    job_id: job_id.clone(),
                    accepted: true,
                    reason: None,
                })?;

                // Track the kind so done_rx can re-send the right RequestJob.
                job_kinds.insert(job_id.clone(), kind.clone());
                // Remove from candidate cache and score baseline — job is no longer pending.
                self.candidates.remove(&job_id);
                self.last_scores.remove(&job_id);

                // Set up abort signal for this job.
                let (abort_tx, abort_rx) = watch::channel(false);
                abort_senders.insert(job_id.clone(), abort_tx);

                // Spawn the job task.
                let executor = self.executor.clone();
                let credentials = self.credentials.clone();
                let job_writer = writer.clone();
                let job_cache_waiters = cache_waiters.clone();
                let job_nar_recv = nar_recv.clone();
                let job_done_tx = done_tx.clone();
                let jid = job_id.clone();

                tokio::spawn(async move {
                    let mut updater = JobUpdater::new(
                        jid.clone(),
                        job_writer,
                        job_cache_waiters,
                        job_nar_recv,
                    );
                    let result = run_job(executor, job, &mut updater, &credentials, abort_rx).await;
                    let _ = job_done_tx.send((jid, result));
                });

                // Chain one more request if still under capacity.
                if active_count + 1 < max {
                    writer.send(ClientMessage::RequestJob { kind })?;
                }
            }

            ServerMessage::AbortJob { job_id, reason } => {
                warn!(%job_id, %reason, "job aborted by server");
                if let Some(tx) = abort_senders.get(&job_id) {
                    let _ = tx.send(true);
                }
            }

            ServerMessage::Credential { kind, data } => {
                debug!(?kind, "received credential");
                self.credentials.store(kind, data);
            }

            ServerMessage::NarPush {
                job_id,
                store_path,
                data,
                offset,
                is_final,
            } => {
                debug!(
                    %job_id,
                    %store_path,
                    offset,
                    is_final,
                    bytes = data.len(),
                    "received NAR chunk from server"
                );
                nar_recv.accept_chunk(&job_id, &store_path, data, is_final);
            }

            ServerMessage::PresignedDownload {
                job_id,
                store_path,
                url: _,
            } => {
                debug!(%job_id, %store_path, "received presigned download URL");
                // TODO(1.4): download NAR from S3 and import into local store.
            }

            ServerMessage::PresignedUpload {
                job_id,
                store_path,
                url,
                method,
                headers,
            } => {
                debug!(%job_id, %store_path, %method, "received presigned upload URL");
                if let Err(e) = crate::proto::nar::upload_presigned(
                    &job_id,
                    &store_path,
                    &url,
                    &method,
                    &headers,
                    writer,
                )
                .await
                {
                    error!(%job_id, %store_path, error = %e, "presigned NAR upload failed");
                }
            }

            ServerMessage::RequestAllScores => {
                debug!(count = self.candidates.len(), "RequestAllScores — re-scoring all cached candidates");
                let all: Vec<JobCandidate> = self.candidates.values().cloned().collect();
                let fresh_scores = if all.is_empty() {
                    vec![]
                } else {
                    self.scorer
                        .score_candidates(&self.executor.store, &all)
                        .await?
                };
                // Send only scores that differ from the server's last known values.
                let changed: Vec<_> = fresh_scores
                    .into_iter()
                    .filter(|s| self.last_scores.get(&s.job_id) != Some(s))
                    .collect();
                for s in &changed {
                    self.last_scores.insert(s.job_id.clone(), s.clone());
                }
                // Always send at least one final chunk so the server knows
                // the worker completed the re-score pass.
                send_score_chunks(writer, changed)?;
            }

            ServerMessage::Draining => {
                info!("server is draining; finishing in-flight work then disconnecting");
                self.draining = true;
            }

            ServerMessage::Error { code, message } => {
                error!(code, %message, "protocol error from server");
            }

            ServerMessage::InitAck { .. } | ServerMessage::Reject { .. } => {
                warn!("unexpected handshake message in dispatch loop — ignoring");
            }

            ServerMessage::AuthChallenge { peers } => {
                debug!(?peers, "mid-connection AuthChallenge — sending AuthResponse");
                let peer_tokens = self.config.peer_tokens();
                let tokens = WorkerConfig::resolve_tokens_for_challenge(&peer_tokens, &peers);
                writer.send(ClientMessage::AuthResponse { tokens })?;
            }

            ServerMessage::AuthUpdate {
                authorized_peers,
                failed_peers,
            } => {
                info!(
                    authorized = authorized_peers.len(),
                    failed = failed_peers.len(),
                    "auth updated"
                );
                for fp in &failed_peers {
                    warn!(peer_id = %fp.peer_id, reason = %fp.reason, "peer auth failed");
                }
            }

            ServerMessage::CacheStatus { job_id, cached } => {
                // Route to the waiting job task, if any.
                if let Some(tx) = cache_waiters.lock().unwrap().remove(&job_id) {
                    let _ = tx.send(cached);
                } else {
                    warn!(%job_id, count = cached.len(), "unexpected CacheStatus — no waiter");
                }
            }
        }

        Ok(())
    }
}

/// Send `scores` as one or more `RequestJobChunk` messages, paginated at
/// 1 000 entries.  The last (or only) message has `is_final: true`.
/// An empty `scores` vec still sends one empty final chunk so the server
/// knows the scoring pass completed.
fn send_score_chunks(
    writer: &crate::connection::ProtoWriter,
    scores: Vec<proto::messages::CandidateScore>,
) -> anyhow::Result<()> {
    if scores.is_empty() {
        writer.send(ClientMessage::RequestJobChunk {
            scores: vec![],
            is_final: true,
        })?;
        return Ok(());
    }
    let chunks: Vec<_> = scores.chunks(1_000).collect();
    let total = chunks.len();
    for (i, chunk) in chunks.into_iter().enumerate() {
        writer.send(ClientMessage::RequestJobChunk {
            scores: chunk.to_vec(),
            is_final: i + 1 == total,
        })?;
    }
    Ok(())
}

/// Short tag for a [`ServerMessage`] used in dispatch-loop timing logs so we
/// can identify which handler is starving the loop without dumping the full
/// message payload (some are large).
fn msg_kind(msg: &ServerMessage) -> &'static str {
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
    }
}

/// Execute a single job inside a spawned task.
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

/// Resolve the worker's persistent UUID.
///
/// Priority:
/// 1. `id_override` — set via `GRADIENT_WORKER_ID` / `--worker-id`; validated as a UUID.
/// 2. `{data_dir}/worker-id` — previously persisted UUID.
/// 3. A freshly generated UUID v4, written to `{data_dir}/worker-id`.
fn load_or_generate_id(data_dir: &str, id_override: Option<&str>) -> Result<String> {
    use std::fs;
    use std::path::Path;

    if let Some(raw) = id_override {
        let id = raw.trim().to_owned();
        id.parse::<uuid::Uuid>()
            .with_context(|| format!("GRADIENT_WORKER_ID is not a valid UUID: {:?}", id))?;
        info!(%id, "using worker ID from GRADIENT_WORKER_ID");
        return Ok(id);
    }

    let dir = Path::new(data_dir);
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create data directory '{}'", data_dir))?;

    let id_path = dir.join("worker-id");

    if id_path.exists() {
        let raw = fs::read_to_string(&id_path)
            .with_context(|| format!("failed to read '{}'", id_path.display()))?;
        let id = raw.trim().to_owned();
        id.parse::<uuid::Uuid>().with_context(|| {
            format!("'{}' contains an invalid UUID: {:?}", id_path.display(), id)
        })?;
        info!(path = %id_path.display(), %id, "loaded persistent worker ID");
        return Ok(id);
    }

    let id = uuid::Uuid::new_v4().to_string();
    fs::write(&id_path, &id).with_context(|| format!("failed to write '{}'", id_path.display()))?;
    info!(path = %id_path.display(), %id, "generated and persisted new worker ID");
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("failed to create temp dir")
    }

    #[test]
    fn load_or_generate_id_creates_new() {
        let dir = temp_dir();
        let data_dir = dir.path().to_string_lossy().to_string();
        let id = load_or_generate_id(&data_dir, None).expect("should generate id");
        id.parse::<uuid::Uuid>()
            .expect("generated id must be a valid UUID");
        let id_path = dir.path().join("worker-id");
        assert!(id_path.exists(), "worker-id file should be created");
        assert_eq!(fs::read_to_string(&id_path).unwrap().trim(), id);
    }

    #[test]
    fn load_or_generate_id_reads_existing() {
        let dir = temp_dir();
        let id_path = dir.path().join("worker-id");
        let known_id = uuid::Uuid::new_v4().to_string();
        fs::write(&id_path, &known_id).unwrap();
        let data_dir = dir.path().to_string_lossy().to_string();
        let loaded = load_or_generate_id(&data_dir, None).expect("should read existing id");
        assert_eq!(loaded, known_id);
    }

    #[test]
    fn load_or_generate_id_invalid_uuid_fails() {
        let dir = temp_dir();
        let id_path = dir.path().join("worker-id");
        fs::write(&id_path, "not-a-uuid").unwrap();
        let data_dir = dir.path().to_string_lossy().to_string();
        let result = load_or_generate_id(&data_dir, None);
        assert!(result.is_err(), "invalid UUID in file should return Err");
    }

    #[test]
    fn load_or_generate_id_override_takes_priority() {
        let dir = temp_dir();
        let id_path = dir.path().join("worker-id");
        let file_id = uuid::Uuid::new_v4().to_string();
        fs::write(&id_path, &file_id).unwrap();
        let override_id = uuid::Uuid::new_v4().to_string();
        let data_dir = dir.path().to_string_lossy().to_string();
        let result =
            load_or_generate_id(&data_dir, Some(&override_id)).expect("override should work");
        assert_eq!(result, override_id, "override must win over file");
    }

    #[test]
    fn load_or_generate_id_override_invalid_uuid_fails() {
        let dir = temp_dir();
        let data_dir = dir.path().to_string_lossy().to_string();
        let result = load_or_generate_id(&data_dir, Some("not-a-uuid"));
        assert!(result.is_err(), "invalid override UUID should return Err");
    }
}
