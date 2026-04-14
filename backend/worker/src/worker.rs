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
//! The `run()` method returns when the connection closes or the server signals
//! `Draining`.  The reconnect loop lives in `main.rs`.

use anyhow::{Context, Result};
use proto::messages::{ClientMessage, Job, ServerMessage};
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
    conn: ProtoConnection,
    executor: JobExecutor,
    scorer: JobScorer,
    credentials: CredentialStore,
    /// Set to `true` after the server sends `Draining`. While draining the
    /// worker finishes any in-flight job but rejects new assignments.
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

        // Record the server's protocol version on the connection object.
        conn.set_server_version(handshake.server_version);
        info!(
            negotiated = ?handshake.negotiated,
            server_version = conn.server_version(),
            "capabilities negotiated"
        );

        // If the server granted build capability, advertise our build capacity.
        if handshake.negotiated.build {
            conn.send(ClientMessage::WorkerCapabilities {
                architectures: vec![], // TODO: detect from nix-daemon
                system_features: vec![],
                max_concurrent_builds: config.max_concurrent_builds,
            })
            .await?;
        }

        // Fetch the initial job candidate list.
        conn.send(ClientMessage::RequestJobList).await?;

        let store = LocalNixStore::connect().await?;
        let evaluator = WorkerEvaluator::new(
            config.eval_workers,
            0, // max_evals_per_worker: 0 = no recycling limit by default
        );
        let credentials = CredentialStore::new();
        let executor = JobExecutor::new(store, evaluator, credentials.clone());

        Ok(Self {
            config,
            conn,
            executor,
            scorer: JobScorer::new(),
            credentials,
            draining: false,
        })
    }

    /// Accept an incoming server-initiated WebSocket connection.
    ///
    /// Called by the listener when `discoverable = true`. Runs the same
    /// handshake and capability negotiation as [`Self::connect`].
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
        let evaluator = WorkerEvaluator::new(config.eval_workers, 0);
        let credentials = CredentialStore::new();
        let executor = JobExecutor::new(store, evaluator, credentials.clone());

        Ok(Self {
            config,
            conn,
            executor,
            scorer: JobScorer::new(),
            credentials,
            draining: false,
        })
    }

    /// Re-use an existing [`ProtoConnection`] that has already been reconnected.
    ///
    /// Called by the reconnect loop in `main.rs` after `reconnect()` succeeds and
    /// the handshake has been re-performed.
    pub async fn reconnect(&mut self) -> Result<()> {
        self.conn.reconnect(&self.config.server_url).await?;
        self.draining = false;

        let peer_id = load_or_generate_id(&self.config.data_dir, self.config.worker_id.as_deref())
            .context("failed to load persistent worker ID")?;
        let peer_tokens = self.config.peer_tokens();

        let handshake = perform_handshake(
            &mut self.conn,
            peer_id,
            peer_tokens,
            self.config.capabilities(),
        )
        .await?;

        self.conn.set_server_version(handshake.server_version);
        info!(
            negotiated = ?handshake.negotiated,
            server_version = self.conn.server_version(),
            "reconnected and re-negotiated capabilities"
        );

        if handshake.negotiated.build {
            self.conn
                .send(ClientMessage::WorkerCapabilities {
                    architectures: vec![],
                    system_features: vec![],
                    max_concurrent_builds: self.config.max_concurrent_builds,
                })
                .await?;
        }

        self.conn.send(ClientMessage::RequestJobList).await?;
        Ok(())
    }

    /// Returns `true` if the server has asked this worker to drain.
    pub fn is_draining(&self) -> bool {
        self.draining
    }

    /// Main dispatch loop — runs until the connection closes or the server
    /// signals `Draining`.  Returns `Ok(())` on a clean disconnect so the
    /// caller can decide whether to reconnect.
    pub async fn run(&mut self) -> Result<()> {
        info!("entering dispatch loop");

        loop {
            let msg = match self.conn.recv().await? {
                Some(m) => m,
                None => {
                    info!("server closed connection");
                    break;
                }
            };

            match msg {
                ServerMessage::JobListChunk {
                    candidates,
                    is_final,
                } => {
                    debug!(
                        count = candidates.len(),
                        is_final, "received job list chunk"
                    );
                    if self.draining {
                        // Don't request any new work while draining.
                        continue;
                    }
                    let scores = self.scorer.score_candidates(&candidates).await?;
                    self.conn
                        .send(ClientMessage::RequestJobChunk { scores, is_final })
                        .await?;
                }

                ServerMessage::JobOffer { candidates } => {
                    debug!(count = candidates.len(), "received job offer");
                    if self.draining {
                        continue;
                    }
                    let scores = self.scorer.score_candidates(&candidates).await?;
                    if !scores.is_empty() {
                        self.conn
                            .send(ClientMessage::RequestJobChunk {
                                scores,
                                is_final: true,
                            })
                            .await?;
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
                        // Politely decline: we are winding down.
                        warn!(%job_id, "rejecting assigned job — draining");
                        self.conn
                            .send(ClientMessage::AssignJobResponse {
                                job_id,
                                accepted: false,
                                reason: Some("worker is draining".to_owned()),
                            })
                            .await?;
                        continue;
                    }

                    info!(%job_id, "job assigned — accepting");
                    self.conn
                        .send(ClientMessage::AssignJobResponse {
                            job_id: job_id.clone(),
                            accepted: true,
                            reason: None,
                        })
                        .await?;

                    let result = self.execute_job(&job_id, job).await;
                    self.credentials.clear();

                    let updater = JobUpdater::new(job_id.clone(), &mut self.conn);
                    match result {
                        Ok(()) => updater.complete().await?,
                        Err(e) => {
                            error!(%job_id, error = %e, "job failed");
                            updater.fail(e.to_string()).await?;
                        }
                    }
                }

                ServerMessage::AbortJob { job_id, reason } => {
                    warn!(%job_id, %reason, "job aborted by server");
                    // TODO: interrupt any in-progress task for this job_id.
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
                        &mut self.conn,
                    )
                    .await
                    {
                        error!(%job_id, %store_path, error = %e, "presigned NAR upload failed");
                    }
                }

                ServerMessage::Draining => {
                    info!("server is draining; finishing in-flight work then disconnecting");
                    self.draining = true;
                    // The loop continues so we can finish any already-accepted job,
                    // but new assignments will be declined above.
                }

                ServerMessage::Error { code, message } => {
                    error!(code, %message, "protocol error from server");
                }

                ServerMessage::InitAck { .. } | ServerMessage::Reject { .. } => {
                    warn!("unexpected handshake message in dispatch loop — ignoring");
                }

                ServerMessage::AuthChallenge { peers } => {
                    debug!(
                        ?peers,
                        "mid-connection AuthChallenge — sending AuthResponse"
                    );
                    let peer_tokens = self.config.peer_tokens();
                    let tokens = WorkerConfig::resolve_tokens_for_challenge(&peer_tokens, &peers);
                    if let Err(e) = self.conn.send(ClientMessage::AuthResponse { tokens }).await {
                        error!(error = %e, "failed to send AuthResponse");
                        break;
                    }
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
            }
        }

        Ok(())
    }

    async fn execute_job(&mut self, job_id: &str, job: Job) -> Result<()> {
        let mut updater = JobUpdater::new(job_id.to_owned(), &mut self.conn);
        match job {
            Job::Flake(flake_job) => {
                self.executor
                    .execute_flake_job(flake_job, &mut updater, &self.credentials)
                    .await
            }
            Job::Build(build_job) => {
                self.executor
                    .execute_build_job(build_job, &mut updater)
                    .await
            }
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
        let result = load_or_generate_id(&data_dir, Some(&override_id)).expect("override should work");
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
