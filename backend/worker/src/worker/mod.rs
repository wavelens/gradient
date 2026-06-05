/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Top-level worker runtime.
//!
//! [`Worker<Connected>`] connects to the Gradient server, performs the
//! handshake, registers capabilities, and then drives the job dispatch loop
//! via [`Worker::run`].
//!
//! The connection state is encoded in the type parameter `S`:
//!
//! - [`Worker<Connected>`] - holds an active [`ProtoConnection`].  Call
//!   [`run`](Worker::run) to enter the dispatch loop.  `run` consumes `self`
//!   and always returns a [`Worker<Disconnected>`] (plus the disconnect
//!   reason) so the caller can decide whether to reconnect.
//!
//! - [`Worker<Disconnected>`] - no active connection.  Call
//!   [`reconnect`](Worker::reconnect) to obtain a fresh `Worker<Connected>`.

mod dispatch;
mod id;
mod scoring;

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use proto::messages::{ClientMessage, JobCandidate, JobKind};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::config::WorkerConfig;
use crate::connection::ProtoConnection;
use crate::connection::handshake::perform_handshake;
use crate::connection_state::{Connected, Disconnected, RunOutcome};
use crate::executor::{JobExecutor, WorkerEvaluator};
use crate::nix::store::LocalNixStore;
use crate::proto::credentials::CredentialStore;
use crate::proto::scorer::JobScorer;

use id::load_or_generate_id;

// ── Worker ────────────────────────────────────────────────────────────────────

/// The worker instance, parameterised by its connection state.
///
/// Create with [`Worker::connect`] (outbound) or [`Worker::from_accepted`]
/// (inbound).  After `run()` returns, call `reconnect()` on the resulting
/// [`Worker<Disconnected>`] to re-establish the connection.
pub struct Worker<S> {
    config: WorkerConfig,
    executor: JobExecutor,
    scorer: JobScorer,
    credentials: CredentialStore,
    /// Local cache of job candidates - updated on `JobListChunk` / `JobOffer`.
    candidates: Arc<Mutex<HashMap<String, JobCandidate>>>,
    /// Last known score per candidate - used for delta filtering.
    last_scores: Arc<Mutex<HashMap<String, proto::messages::CandidateScore>>>,
    /// Connection state: [`Connected`] or [`Disconnected`].
    conn_state: S,
    _marker: PhantomData<S>,
}

impl<S> Worker<S> {
    /// Cheap clone of the executor so callers can keep a shutdown handle
    /// alive across `Worker` consumption (e.g. across `run` / `reconnect`
    /// boundaries) and call [`JobExecutor::shutdown`] to gracefully drain
    /// the eval pool. The internal `Arc<WorkerEvaluator>` is reference-
    /// counted, so the underlying pool stays alive as long as any clone
    /// exists.
    pub fn executor_handle(&self) -> JobExecutor {
        self.executor.clone()
    }
}

// ── Constructors (→ Worker<Connected>) ───────────────────────────────────────

impl Worker<Connected> {
    /// Connect to the server at `config.server_url`, complete the handshake,
    /// and advertise build capabilities.
    pub async fn connect(config: WorkerConfig) -> Result<Self> {
        let mut conn = ProtoConnection::open(&config.server_url).await?;
        Self::setup_connection(&mut conn, &config).await?;
        let (executor, scorer) = Self::build_executor(&config).await?;
        Ok(Self::new_connected(config, conn, executor, scorer))
    }

    /// Accept an incoming server-initiated WebSocket connection.
    pub async fn from_accepted(
        ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        config: WorkerConfig,
    ) -> Result<Self> {
        let mut conn = ProtoConnection::from_accepted(ws);
        Self::setup_connection_incoming(&mut conn, &config).await?;
        let (executor, scorer) = Self::build_executor(&config).await?;
        Ok(Self::new_connected(config, conn, executor, scorer))
    }

    fn new_connected(
        config: WorkerConfig,
        conn: ProtoConnection,
        executor: JobExecutor,
        scorer: JobScorer,
    ) -> Self {
        Self {
            config,
            executor,
            scorer,
            credentials: CredentialStore::new(),
            candidates: Arc::new(Mutex::new(HashMap::new())),
            last_scores: Arc::new(Mutex::new(HashMap::new())),
            conn_state: Connected { conn },
            _marker: PhantomData,
        }
    }
}

// ── Reconnect (Worker<Disconnected> → Worker<Connected>) ─────────────────────

impl Worker<Disconnected> {
    /// Re-open the connection to the server and return a `Worker<Connected>`.
    ///
    /// Preserves the executor, scorer, credentials, and job caches from the
    /// previous connection so reconnects are cheap.
    ///
    /// On failure, returns the original `Worker<Disconnected>` alongside the
    /// error so the caller can retry with backoff without losing the cached
    /// executor/scorer/credentials state.
    pub async fn reconnect(self) -> std::result::Result<Worker<Connected>, (anyhow::Error, Self)> {
        let mut conn = match ProtoConnection::open(&self.config.server_url).await {
            Ok(c) => c,
            Err(e) => return Err((e, self)),
        };
        // Reuse the same handshake + capability-advertise + initial-request
        // sequence as a fresh connect - the server has no prior state for
        // this worker after a restart, so reconnect must re-publish
        // architectures, system_features, and max_concurrent_builds.
        if let Err(e) = perform_setup(&mut conn, &self.config, "reconnect").await {
            return Err((e, self));
        }

        let Worker {
            config,
            executor,
            scorer,
            credentials,
            candidates,
            last_scores,
            ..
        } = self;

        Ok(Worker {
            config,
            executor,
            scorer,
            credentials,
            candidates,
            last_scores,
            conn_state: Connected { conn },
            _marker: PhantomData,
        })
    }
}

// ── Dispatch loop (Worker<Connected> → Worker<Disconnected>) ─────────────────

impl Worker<Connected> {
    /// Main dispatch loop.
    ///
    /// Consumes `self` (takes ownership of the connection), drives the loop,
    /// and always returns a [`Worker<Disconnected>`] plus the [`RunOutcome`].
    ///
    /// On an `Err` outcome the disconnected worker is still returned so the
    /// caller can reconnect without losing the executor / credential caches.
    ///
    /// `shutdown` is observed by the inner `select!`; cancelling it causes
    /// the loop to exit cleanly with [`RunOutcome::CleanDisconnect`].
    pub async fn run(
        self,
        shutdown: CancellationToken,
    ) -> (Worker<Disconnected>, Result<RunOutcome>) {
        let Worker {
            config,
            executor,
            scorer,
            credentials,
            candidates,
            last_scores,
            conn_state: Connected { conn },
            ..
        } = self;

        let mut credentials = credentials;

        let outcome = dispatch::run_dispatch_loop(
            conn,
            &config,
            &executor,
            scorer,
            &mut credentials,
            &candidates,
            &last_scores,
            shutdown,
        )
        .await;

        let disconnected = Worker {
            config,
            executor,
            scorer,
            credentials,
            candidates,
            last_scores,
            conn_state: Disconnected,
            _marker: PhantomData,
        };

        let result = outcome.map(|drained| {
            if drained {
                RunOutcome::Drained
            } else {
                RunOutcome::CleanDisconnect
            }
        });

        (disconnected, result)
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

impl Worker<Connected> {
    async fn setup_connection(conn: &mut ProtoConnection, config: &WorkerConfig) -> Result<()> {
        perform_setup(conn, config, "outbound").await
    }

    async fn setup_connection_incoming(
        conn: &mut ProtoConnection,
        config: &WorkerConfig,
    ) -> Result<()> {
        perform_setup(conn, config, "inbound").await
    }

    async fn build_executor(config: &WorkerConfig) -> Result<(JobExecutor, JobScorer)> {
        let store = LocalNixStore::connect(config.max_nixdaemon_connections)?;
        let evaluator = WorkerEvaluator::new(config.eval_workers, config.max_evals_per_worker);
        let gcroots = crate::nix::gcroots::GcRootKeeper::new(
            &config.gcroots_dir,
            std::sync::Arc::new(store.clone()),
        );
        if let Err(e) = gcroots.purge_all().await {
            tracing::warn!(error = %e, "gcroot purge failed at startup; continuing");
        }
        let executor = JobExecutor::new(
            store,
            evaluator,
            gcroots,
            config.binpath_nix.clone(),
            config.binpath_ssh.clone(),
            config.build_metrics,
            config.build_cgroup_root.clone(),
            crate::executor::log_limit::LogRateLimits {
                burst_bytes_per_min: config.log_burst_bytes_per_min,
                sustained_bytes_per_hour: config.log_sustained_bytes_per_hour,
            },
            config.log_fetch_from_store,
        );
        Ok((executor, JobScorer::new()))
    }
}

/// Shared setup: perform handshake, advertise capabilities, request initial job list.
async fn perform_setup(
    conn: &mut ProtoConnection,
    config: &WorkerConfig,
    direction: &str,
) -> Result<()> {
    let peer_id = load_or_generate_id(&config.data_dir, config.worker_id.as_deref())
        .context("failed to load or generate persistent worker ID")?;
    let peer_tokens = config.peer_tokens();
    let handshake = perform_handshake(conn, peer_id, peer_tokens, config.capabilities()).await?;
    conn.set_server_version(handshake.server_version);
    info!(
        direction,
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
        let host = crate::metrics::host_static();
        let cpu_core_score = config
            .cpu_core_score
            .unwrap_or_else(crate::metrics::cpu_core_score);
        info!(
            ?architectures,
            ?system_features,
            max_concurrent_builds = config.max_concurrent_builds,
            cpu_count = host.cpu_count,
            ram_total_mb = host.ram_total_mb,
            cpu_core_score,
            "advertising build capabilities"
        );
        conn.send(ClientMessage::WorkerCapabilities {
            architectures,
            system_features,
            max_concurrent_builds: config.max_concurrent_builds,
            cpu_count: host.cpu_count,
            ram_total_mb: host.ram_total_mb,
            cpu_core_score,
        })
        .await?;
    }
    conn.send(ClientMessage::RequestJobList).await?;
    conn.send(ClientMessage::RequestJob {
        kind: JobKind::Flake,
    })
    .await?;
    conn.send(ClientMessage::RequestJob {
        kind: JobKind::Build,
    })
    .await?;
    Ok(())
}
