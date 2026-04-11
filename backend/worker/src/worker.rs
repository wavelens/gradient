/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Top-level worker runtime.
//!
//! [`Worker`] connects to the Gradient server, performs the handshake,
//! registers capabilities, and then drives the job dispatch loop.

use anyhow::Result;
use proto::messages::{ClientMessage, Job, ServerMessage};
use tracing::{debug, error, info, warn};

use crate::config::WorkerConfig;
use crate::connection::ProtoConnection;
use crate::credentials::CredentialStore;
use crate::executor::{JobExecutor, WorkerEvaluator};
use crate::handshake::perform_handshake;
use crate::job::JobUpdater;
use crate::scorer::JobScorer;
use crate::store::LocalNixStore;

/// The running worker instance.
pub struct Worker {
    config: WorkerConfig,
    conn: ProtoConnection,
    executor: JobExecutor,
    scorer: JobScorer,
    credentials: CredentialStore,
}

impl Worker {
    /// Connect to the server at `config.server_url`, complete the handshake,
    /// and advertise build capabilities.
    pub async fn connect(config: WorkerConfig) -> Result<Self> {
        let mut conn = ProtoConnection::open(&config.server_url).await?;

        // TODO: load persistent peer_id from disk; generate on first start.
        let peer_id = uuid::Uuid::new_v4().to_string();
        let token = read_token(config.token_file.as_deref()).await?;

        let handshake = perform_handshake(
            &mut conn,
            peer_id,
            token,
            config.capabilities(),
        )
        .await?;

        info!(negotiated = ?handshake.negotiated, "capabilities negotiated");

        // If the server granted build capability, advertise our build capacity.
        if handshake.negotiated.build {
            conn.send(ClientMessage::WorkerCapabilities {
                architectures: vec![], // TODO(1.3): detect from nix-daemon
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
        })
    }

    /// Main dispatch loop — runs until the connection is closed.
    pub async fn run(&mut self) -> Result<()> {
        info!("entering dispatch loop");

        loop {
            let msg = match self.conn.recv().await? {
                Some(m) => m,
                None => {
                    info!("server closed connection; will reconnect");
                    break;
                }
            };

            match msg {
                ServerMessage::JobListChunk { candidates, is_final } => {
                    debug!(count = candidates.len(), is_final, "received job list chunk");
                    let scores = self.scorer.score_candidates(&candidates).await?;
                    self.conn
                        .send(ClientMessage::RequestJobChunk {
                            scores,
                            is_final,
                        })
                        .await?;
                }

                ServerMessage::JobOffer { candidates } => {
                    debug!(count = candidates.len(), "received job offer");
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
                    // TODO: remove from local candidate cache.
                }

                ServerMessage::AssignJob { job_id, job, timeout_secs: _ } => {
                    info!(%job_id, "job assigned — accepting");
                    self.conn
                        .send(ClientMessage::AssignJobResponse {
                            job_id: job_id.clone(),
                            accepted: true,
                            reason: None,
                        })
                        .await?;

                    // Execute the job. Each task sends its own progress updates.
                    let result = self.execute_job(&job_id, job).await;

                    // Clear credentials now that the job is done.
                    self.credentials.clear();

                    // Report completion or failure.
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

                ServerMessage::NarPush { job_id, store_path, data: _, offset, is_final } => {
                    debug!(%job_id, %store_path, offset, is_final, "received NAR chunk from server");
                    // TODO(1.4): reassemble and import into local nix store.
                }

                ServerMessage::PresignedDownload { job_id, store_path, url: _ } => {
                    debug!(%job_id, %store_path, "received presigned download URL");
                    // TODO(1.4): download NAR from S3 and import.
                }

                ServerMessage::PresignedUpload { .. } => {
                    // Server should not send this unsolicited.
                    warn!("unexpected PresignedUpload — ignoring");
                }

                ServerMessage::Draining => {
                    info!("server is draining; finishing in-flight work then disconnecting");
                    // TODO: stop accepting new jobs, finish current.
                    break;
                }

                ServerMessage::Error { code, message } => {
                    error!(code, %message, "protocol error from server");
                }

                ServerMessage::InitAck { .. } | ServerMessage::Reject { .. } => {
                    warn!("unexpected handshake message in dispatch loop — ignoring");
                }
            }
        }

        Ok(())
    }

    async fn execute_job(&mut self, job_id: &str, job: Job) -> Result<()> {
        let mut updater = JobUpdater::new(job_id.to_owned(), &mut self.conn);
        match job {
            Job::Flake(flake_job) => {
                self.executor.execute_flake_job(flake_job, &mut updater).await
            }
            Job::Build(build_job) => {
                self.executor.execute_build_job(build_job, &mut updater).await
            }
        }
    }
}

/// Read the API token from the file at `path`, if any.
async fn read_token(path: Option<&str>) -> Result<Option<String>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let content = tokio::fs::read_to_string(path).await?;
    Ok(Some(content.trim().to_owned()))
}
