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
    ///
    /// `Arc<Mutex<…>>` so scoring can run in a spawned task without blocking
    /// the dispatch loop. Critical sections are short (HashMap insert/remove)
    /// — std::sync::Mutex is sufficient and keeps the locking sync.
    candidates: Arc<std::sync::Mutex<HashMap<String, proto::messages::JobCandidate>>>,
    /// Last known score per candidate (keyed by job_id).
    /// Used to filter `RequestJobChunk` to only changed scores.
    last_scores: Arc<std::sync::Mutex<HashMap<String, proto::messages::CandidateScore>>>,
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
            candidates: Arc::new(std::sync::Mutex::new(HashMap::new())),
            last_scores: Arc::new(std::sync::Mutex::new(HashMap::new())),
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
            candidates: Arc::new(std::sync::Mutex::new(HashMap::new())),
            last_scores: Arc::new(std::sync::Mutex::new(HashMap::new())),
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
                // Cache candidates synchronously (fast — HashMap insert) so
                // future RequestAllScores / RevokeJob handlers see them
                // immediately. The actual `score_candidates` (which reads
                // many `.drv` files) runs in a spawned task so this handler
                // returns to the dispatch loop's `select!` right away —
                // critical for keeping `heartbeat` / `done_rx` /
                // `CacheStatus` polling responsive on big initial snapshots.
                {
                    let mut g = self.candidates.lock().unwrap();
                    for c in &candidates {
                        g.insert(c.job_id.clone(), c.clone());
                    }
                }
                spawn_scoring_task(
                    self.scorer,
                    Arc::clone(&self.executor.store),
                    Arc::clone(&self.last_scores),
                    writer.clone(),
                    candidates,
                    /* delta_filter = */ false, // first time: send all
                    /* is_final = */ is_final,
                );
            }

            ServerMessage::JobOffer { candidates } => {
                debug!(count = candidates.len(), "received job offer");
                if self.draining {
                    return Ok(());
                }
                // Filter to genuinely new/changed candidates (the server sends
                // delta-only offers but guard against duplicate frames),
                // record them in the local cache, then spawn scoring off the
                // dispatch loop. The dispatch loop returns immediately —
                // even if scoring takes minutes, heartbeat ticks keep firing
                // and `CacheStatus` replies for an in-flight eval still get
                // routed without delay.
                let new_candidates: Vec<JobCandidate> = {
                    let mut g = self.candidates.lock().unwrap();
                    let mut out = Vec::with_capacity(candidates.len());
                    for c in candidates {
                        if g.get(&c.job_id) != Some(&c) {
                            g.insert(c.job_id.clone(), c.clone());
                            out.push(c);
                        }
                    }
                    out
                };
                if !new_candidates.is_empty() {
                    spawn_scoring_task(
                        self.scorer,
                        Arc::clone(&self.executor.store),
                        Arc::clone(&self.last_scores),
                        writer.clone(),
                        new_candidates,
                        /* delta_filter = */ true,
                        /* is_final = */ true,
                    );
                }
            }

            ServerMessage::RevokeJob { job_ids } => {
                debug!(?job_ids, "jobs revoked");
                let mut cands = self.candidates.lock().unwrap();
                let mut scores = self.last_scores.lock().unwrap();
                for id in &job_ids {
                    cands.remove(id);
                    scores.remove(id);
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
                self.candidates.lock().unwrap().remove(&job_id);
                self.last_scores.lock().unwrap().remove(&job_id);

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
                let all: Vec<JobCandidate> = {
                    let g = self.candidates.lock().unwrap();
                    g.values().cloned().collect()
                };
                debug!(count = all.len(), "RequestAllScores — re-scoring all cached candidates");
                if all.is_empty() {
                    // Server still expects exactly one final empty chunk so
                    // it knows the re-score pass completed. Send it inline —
                    // there's nothing to score, so no spawn needed.
                    send_score_chunks(writer, vec![])?;
                } else {
                    spawn_scoring_task(
                        self.scorer,
                        Arc::clone(&self.executor.store),
                        Arc::clone(&self.last_scores),
                        writer.clone(),
                        all,
                        /* delta_filter = */ true,
                        /* is_final = */ true,
                    );
                }
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
                    // Benign race: a CacheQuery reply arrives after the
                    // job has already completed and the dispatch loop's
                    // done_rx handler removed the waiter (or after the
                    // 120s `query_cache` timeout dropped it). The log info
                    // is no longer needed since the requesting code path
                    // is gone — drop the response and move on. Stays at
                    // debug because it's noisy and not actionable.
                    debug!(%job_id, count = cached.len(), "CacheStatus arrived after waiter cleared (race with job completion or timeout)");
                }
            }
        }

        Ok(())
    }
}

/// Drive a `score_candidates` pass off the dispatch loop.
///
/// Spawns a tokio task that:
///   1. scores `candidates` against the local store,
///   2. (optionally) filters down to scores that differ from the cached
///      `last_scores` baseline,
///   3. updates `last_scores` with the surviving entries (under the shared
///      mutex), and
///   4. emits one or more `RequestJobChunk` frames via `writer`.
///
/// This is the load-bearing fix for "no heartbeats during eval": before this,
/// each `JobOffer` / `JobListChunk` / `RequestAllScores` ran the full
/// `.drv`-reading scoring pass inside `handle_message.await`, holding the
/// dispatch loop out of `select!` for as long as scoring took. With the
/// scoring spawned, the dispatch loop returns immediately, so heartbeat
/// ticks fire on schedule and `CacheStatus` replies for an in-flight eval
/// are routed without delay.
///
/// `delta_filter = true` matches `JobOffer` / `RequestAllScores` semantics
/// (only resend changed scores). `delta_filter = false` matches the initial
/// `JobListChunk` snapshot (record every score as the baseline).
/// `is_final` controls the `RequestJobChunk { is_final: … }` flag on the
/// last chunk emitted in this pass.
fn spawn_scoring_task(
    scorer: JobScorer,
    store: Arc<crate::nix::store::LocalNixStore>,
    last_scores: Arc<std::sync::Mutex<HashMap<String, proto::messages::CandidateScore>>>,
    writer: crate::connection::ProtoWriter,
    candidates: Vec<JobCandidate>,
    delta_filter: bool,
    is_final: bool,
) {
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        let count = candidates.len();
        let scores = match scorer.score_candidates(&store, &candidates).await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, count, "score_candidates failed in spawned task");
                return;
            }
        };

        // Filter to changed (vs baseline) when requested, then update the
        // baseline with whatever we actually emit. The mutex section is
        // short — HashMap lookup + insert per surviving score.
        let to_send: Vec<proto::messages::CandidateScore> = {
            let mut g = last_scores.lock().unwrap();
            let mut out = Vec::with_capacity(scores.len());
            for s in scores {
                if !delta_filter || g.get(&s.job_id) != Some(&s) {
                    g.insert(s.job_id.clone(), s.clone());
                    out.push(s);
                }
            }
            out
        };

        debug!(
            scored = count,
            sending = to_send.len(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            is_final,
            "scoring task complete"
        );

        // Send the resulting RequestJobChunk frames. Mirrors the original
        // inline logic: paginate at 1 000 per frame, with `is_final` on the
        // last chunk; non-final passes (mid-stream JobListChunk) flag every
        // chunk as is_final=false.
        if is_final {
            if let Err(e) = send_score_chunks(&writer, to_send) {
                warn!(error = %e, "send_score_chunks (final) failed");
            }
        } else {
            for chunk in to_send.chunks(1_000) {
                if let Err(e) = writer.send(ClientMessage::RequestJobChunk {
                    scores: chunk.to_vec(),
                    is_final: false,
                }) {
                    warn!(error = %e, "send RequestJobChunk (non-final) failed");
                    break;
                }
            }
        }
    });
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
