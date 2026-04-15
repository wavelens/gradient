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
use proto::messages::{CachedPath, ClientMessage, Job, ServerMessage};
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
            conn.send(ClientMessage::WorkerCapabilities {
                architectures: vec![],
                system_features: vec![],
                max_concurrent_builds: config.max_concurrent_builds,
            })
            .await?;
        }

        conn.send(ClientMessage::RequestJobList).await?;

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
            conn.send(ClientMessage::WorkerCapabilities {
                architectures: vec![],
                system_features: vec![],
                max_concurrent_builds: config.max_concurrent_builds,
            })
            .await?;
        }

        conn.send(ClientMessage::RequestJobList).await?;

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

        // Abort senders: keyed by job_id; job tasks hold the receiver.
        let mut abort_senders: HashMap<String, watch::Sender<bool>> = HashMap::new();

        // Channel used by job tasks to report completion back to the dispatch loop.
        let (done_tx, mut done_rx) =
            mpsc::unbounded_channel::<(String, Result<()>)>();

        info!("entering dispatch loop");

        loop {
            tokio::select! {
                biased;

                // A job task finished — send the result to the server.
                Some((job_id, result)) = done_rx.recv() => {
                    abort_senders.remove(&job_id);
                    cache_waiters.lock().unwrap().remove(&job_id);
                    self.credentials.clear();

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
                        &mut abort_senders,
                        &done_tx,
                    ).await?;
                }
            }
        }

        Ok(())
    }

    /// Handle a single [`ServerMessage`] inside the dispatch loop.
    async fn handle_message(
        &mut self,
        msg: ServerMessage,
        writer: &crate::connection::ProtoWriter,
        cache_waiters: &Arc<Mutex<HashMap<String, oneshot::Sender<Vec<CachedPath>>>>>,
        abort_senders: &mut HashMap<String, watch::Sender<bool>>,
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
                let scores = self
                    .scorer
                    .score_candidates(&self.executor.store, &candidates)
                    .await?;
                writer.send(ClientMessage::RequestJobChunk { scores, is_final })?;
            }

            ServerMessage::JobOffer { candidates } => {
                debug!(count = candidates.len(), "received job offer");
                if self.draining {
                    return Ok(());
                }
                let scores = self
                    .scorer
                    .score_candidates(&self.executor.store, &candidates)
                    .await?;
                if !scores.is_empty() {
                    writer.send(ClientMessage::RequestJobChunk {
                        scores,
                        is_final: true,
                    })?;
                }
            }

            ServerMessage::RevokeJob { job_ids } => {
                debug!(?job_ids, "jobs revoked");
                // TODO: cancel any locally-queued (not yet started) candidates.
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

                info!(%job_id, "job assigned — accepting");
                writer.send(ClientMessage::AssignJobResponse {
                    job_id: job_id.clone(),
                    accepted: true,
                    reason: None,
                })?;

                // Set up abort signal for this job.
                let (abort_tx, abort_rx) = watch::channel(false);
                abort_senders.insert(job_id.clone(), abort_tx);

                // Spawn the job task.
                let executor = self.executor.clone();
                let credentials = self.credentials.clone();
                let job_writer = writer.clone();
                let job_cache_waiters = cache_waiters.clone();
                let job_done_tx = done_tx.clone();
                let jid = job_id.clone();

                tokio::spawn(async move {
                    let mut updater = JobUpdater::new(jid.clone(), job_writer, job_cache_waiters);
                    let result = run_job(executor, job, &mut updater, &credentials, abort_rx).await;
                    let _ = job_done_tx.send((jid, result));
                });
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
                data: _,
                offset,
                is_final,
            } => {
                debug!(%job_id, %store_path, offset, is_final, "received NAR chunk from server");
                // TODO(1.4): reassemble and import into local nix store.
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
