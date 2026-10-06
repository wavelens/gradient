/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod cluster;
mod id;
mod message_loop;
mod scoring;

use std::marker::PhantomData;

use anyhow::{Context, Result};
use gradient_wire::messages::{ClientMessage, JobKind};
use tracing::info;

use crate::config::WorkerConfig;
use crate::connection_state::{Connected, Disconnected};
use crate::executor::{JobExecutor, WorkerEvaluator};
use crate::nix::store::LocalNixStore;
use crate::proto::credentials::CredentialStore;
use crate::proto::scorer::JobScorer;
use gradient_worker_client::connection::ProtoConnection;
use gradient_worker_client::connection::handshake::{perform_dialed_handshake, perform_handshake};
use gradient_worker_client::reconnect::RunOutcome;
use tokio_util::sync::CancellationToken;

use id::load_or_generate_id;

const WRITER_FLUSH_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

pub struct Worker<S> {
    config: WorkerConfig,
    executor: JobExecutor,
    scorer: JobScorer,
    credentials: CredentialStore,
    conn_state: S,
    _marker: PhantomData<S>,
}

impl<S> Worker<S> {
    pub fn executor_handle(&self) -> JobExecutor {
        self.executor.clone()
    }
}

impl Worker<Connected> {
    pub async fn connect(config: WorkerConfig) -> Result<Self> {
        let mut conn = ProtoConnection::open(&config.server_url).await?;
        Self::setup_connection(&mut conn, &config).await?;
        let (executor, scorer) = Self::build_executor(&config).await?;
        Ok(Self::new_connected(config, conn, executor, scorer))
    }

    pub async fn from_accepted(
        ws: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
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
            conn_state: Connected { conn },
            _marker: PhantomData,
        }
    }
}

impl Worker<Disconnected> {
    /// The server is holding no state for this worker after a restart.
    /// A reconnect must re-run the full handshake, capability advertisement and initial request.
    pub async fn reconnect(&self) -> Result<ProtoConnection> {
        let mut conn = ProtoConnection::open(&self.config.server_url).await?;
        perform_setup(&mut conn, &self.config, Side::Reconnect).await?;
        Ok(conn)
    }

    pub fn into_connected(self, conn: ProtoConnection) -> Worker<Connected> {
        let Worker {
            config,
            executor,
            scorer,
            credentials,
            ..
        } = self;

        Worker {
            config,
            executor,
            scorer,
            credentials,
            conn_state: Connected { conn },
            _marker: PhantomData,
        }
    }
}

impl Worker<Connected> {
    pub async fn run(self, stop: CancellationToken) -> (Worker<Disconnected>, Result<RunOutcome>) {
        let Worker {
            config,
            executor,
            scorer,
            credentials,
            conn_state: Connected { conn },
            ..
        } = self;

        let (writer, reader, flush) = conn.split();
        let state = message_loop::MessageLoopState::new(
            writer,
            config.clone(),
            executor.clone(),
            scorer,
            credentials.clone(),
        );
        let outcome = message_loop::run_message_loop(state, reader, stop.clone()).await;

        // Only background tasks can still hold a writer clone at this point.
        // A stopping worker must see its last reports leave the queue.
        // A session ending for a reconnect is closing at once.
        if stop.is_cancelled() {
            flush.flush(WRITER_FLUSH_BUDGET).await;
        } else {
            flush.close();
        }

        let disconnected = Worker {
            config,
            executor,
            scorer,
            credentials,
            conn_state: Disconnected,
            _marker: PhantomData,
        };

        let result = outcome.map(|end| match end {
            _ if end.draining => RunOutcome::Drained,
            _ if end.refused => RunOutcome::Refused,
            _ => RunOutcome::CleanDisconnect,
        });

        (disconnected, result)
    }
}

impl Worker<Connected> {
    async fn setup_connection(conn: &mut ProtoConnection, config: &WorkerConfig) -> Result<()> {
        perform_setup(conn, config, Side::Outbound).await
    }

    async fn setup_connection_incoming(
        conn: &mut ProtoConnection,
        config: &WorkerConfig,
    ) -> Result<()> {
        perform_setup(conn, config, Side::Inbound).await
    }

    async fn build_executor(config: &WorkerConfig) -> Result<(JobExecutor, JobScorer)> {
        let store = LocalNixStore::connect(config.nix_daemon.max_connections)?;
        let evaluator = WorkerEvaluator::new(
            config.eval.fork_workers,
            config.eval.max_rss,
            config.system.min_free_ram_mb,
            config.eval_cache_dir(),
            config.eval.cache_share,
        );
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
            config.ssh_bin.clone(),
            crate::executor::log_limit::LogRateLimits {
                burst_bytes_per_min: config.log.burst_bytes_per_min,
                sustained_bytes_per_hour: config.log.sustained_bytes_per_hour,
            },
            config.log.fetch_from_store,
            crate::executor::BuildHost {
                build_cores: config.build_cores(),
                cpu_core_score: config.cpu_core_score(),
            },
        );
        Ok((executor, JobScorer::new()))
    }
}

#[derive(Clone, Copy, Debug)]
enum Side {
    Outbound,
    Reconnect,
    Inbound,
}

async fn perform_setup(
    conn: &mut ProtoConnection,
    config: &WorkerConfig,
    side: Side,
) -> Result<()> {
    let peer_id = load_or_generate_id(&config.base_dir, config.id.as_deref())
        .context("failed to load or generate persistent worker ID")?;
    let handshake = match side {
        Side::Inbound => {
            perform_dialed_handshake(
                conn,
                peer_id,
                config.accepted_server_tokens(),
                config.capabilities(),
            )
            .await?
        }
        Side::Outbound | Side::Reconnect => {
            perform_handshake(conn, peer_id, config.peer_tokens(), config.capabilities()).await?
        }
    };
    info!(
        ?side,
        negotiated = ?handshake.negotiated,
        version = handshake.version,
        "capabilities negotiated"
    );
    if handshake.negotiated.build {
        let architectures = config
            .system
            .architectures
            .clone()
            .unwrap_or_else(|| vec![crate::config::host_system()]);
        let system_features = match config.system.features.clone() {
            Some(features) => features,
            None => detect_system_features(&config.nix_bin).await,
        };
        let host = crate::metrics::host_static();
        let cpu_core_score = config.cpu_core_score();
        info!(
            ?architectures,
            ?system_features,
            max_concurrent_builds = config.build.max_concurrent,
            cpu_count = host.cpu_count,
            ram_total_mb = host.ram_total_mb,
            cpu_core_score,
            "advertising build capabilities"
        );
        conn.send(ClientMessage::WorkerCapabilities {
            architectures,
            system_features,
            max_concurrent_builds: config.build.max_concurrent,
            cpu_count: host.cpu_count,
            ram_total_mb: host.ram_total_mb,
            cpu_core_score,
            zone: config.zone.clone().filter(|z| !z.is_empty()),
            endpoint: config.endpoint.clone().filter(|e| !e.is_empty()),
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

fn parse_system_features(output: &str) -> Vec<String> {
    let value = output.split_once('=').map_or(output, |(_, v)| v);
    value.split_whitespace().map(str::to_owned).collect()
}

async fn detect_system_features(binpath_nix: &str) -> Vec<String> {
    let output = tokio::process::Command::new(binpath_nix)
        .args([
            "--extra-experimental-features",
            "nix-command",
            "config",
            "show",
            "system-features",
        ])
        .output()
        .await;
    match output {
        Ok(o) if o.status.success() => parse_system_features(&String::from_utf8_lossy(&o.stdout)),
        Ok(o) => {
            tracing::warn!(
                status = ?o.status.code(),
                stderr = %String::from_utf8_lossy(&o.stderr).trim(),
                "`nix config show system-features` failed; advertising no system features"
            );
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(error = %e, binpath_nix, "could not run nix to detect system features; advertising none");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_system_features;

    #[test]
    fn parses_space_separated_value() {
        let out = "benchmark big-parallel gccarch-x86-64-v2 kvm nixos-test\n";
        assert_eq!(
            parse_system_features(out),
            vec![
                "benchmark",
                "big-parallel",
                "gccarch-x86-64-v2",
                "kvm",
                "nixos-test"
            ]
        );
    }

    #[test]
    fn tolerates_name_equals_value_form() {
        let out = "system-features = kvm big-parallel\n";
        assert_eq!(parse_system_features(out), vec!["kvm", "big-parallel"]);
    }

    #[test]
    fn empty_output_yields_no_features() {
        assert!(parse_system_features("\n").is_empty());
    }
}
