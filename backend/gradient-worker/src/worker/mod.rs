/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod cluster;
mod id;
mod jobs;
mod message_loop;
mod scoring;

use std::marker::PhantomData;
use std::sync::Arc;

use anyhow::{Context, Result};
use gradient_wire::messages::{ClientMessage, JobKind, RUNNING_JOBS_SINCE, RunningAssignment};
use tracing::{info, warn};

use crate::config::WorkerConfig;
use crate::connection_state::{Connected, Disconnected};
use crate::exec::host::ExecHost;
use crate::exec::reattach::ResumedJob;
use crate::executor::{JobExecutor, WorkerEvaluator};
use crate::nix::store::LocalNixStore;
use crate::proto::credentials::CredentialStore;
use crate::proto::scorer::JobScorer;
use gradient_worker_client::connection::ProtoConnection;
use gradient_worker_client::connection::handshake::{perform_dialed_handshake, perform_handshake};
use gradient_worker_client::reconnect::RunOutcome;
use tokio_util::sync::CancellationToken;

pub(crate) use id::load_or_generate_id;
use jobs::WorkerJobs;

pub(crate) use jobs::clear_job_logs;

const WRITER_FLUSH_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

const NO_JOB_SESSION: u64 = 0;

pub struct Worker<S> {
    config: WorkerConfig,
    executor: JobExecutor,
    scorer: JobScorer,
    credentials: CredentialStore,
    jobs: Arc<WorkerJobs>,
    conn_state: S,
    _marker: PhantomData<S>,
}

impl<S> Worker<S> {
    pub fn executor_handle(&self) -> JobExecutor {
        self.executor.clone()
    }
}

impl Worker<Connected> {
    pub async fn from_accepted(
        ws: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        config: WorkerConfig,
        exec: Option<Arc<ExecHost>>,
    ) -> Result<Self> {
        let mut conn = ProtoConnection::from_accepted(ws);
        perform_setup(
            &mut conn,
            &config,
            Side::Inbound,
            Vec::new(),
            exec.as_deref(),
        )
        .await?;
        let (executor, scorer) = build_executor(&config, exec).await?;
        Ok(Self::new_connected(config, conn, executor, scorer))
    }

    fn new_connected(
        config: WorkerConfig,
        conn: ProtoConnection,
        executor: JobExecutor,
        scorer: JobScorer,
    ) -> Self {
        Self {
            jobs: Arc::new(WorkerJobs::new(&config)),
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
    pub async fn start(
        config: WorkerConfig,
        exec: Option<Arc<ExecHost>>,
        resumed: Vec<ResumedJob>,
    ) -> Result<Self> {
        let (executor, scorer) = build_executor(&config, exec).await?;
        let jobs = Arc::new(WorkerJobs::new(&config));
        for job in resumed {
            resume_exec_job(&jobs, &executor, job);
        }
        Ok(Self {
            config,
            executor,
            scorer,
            credentials: CredentialStore::new(),
            jobs,
            conn_state: Disconnected,
            _marker: PhantomData,
        })
    }

    pub async fn connect(&self, server_url: &str) -> Result<ProtoConnection> {
        let mut conn = ProtoConnection::open(server_url).await?;
        perform_setup(
            &mut conn,
            &self.config,
            Side::Outbound,
            self.jobs.running_assignments(),
            self.executor.exec.as_deref(),
        )
        .await?;
        Ok(conn)
    }

    pub fn into_connected(self, conn: ProtoConnection) -> Worker<Connected> {
        let Worker {
            config,
            executor,
            scorer,
            credentials,
            jobs,
            ..
        } = self;

        Worker {
            config,
            executor,
            scorer,
            credentials,
            jobs,
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
            jobs,
            conn_state: Connected { conn },
            ..
        } = self;

        let (writer, reader, flush) = conn.split();
        let version = writer.version();
        let job_session = jobs
            .start_session(writer.clone())
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "the new session closed before the queued job reports went out");
                NO_JOB_SESSION
            });
        let state = message_loop::MessageLoopState::new(
            writer,
            &jobs,
            config.clone(),
            executor.clone(),
            scorer,
            credentials.clone(),
        );
        let outcome =
            message_loop::run_message_loop(state, reader, job_session, stop.clone()).await;
        jobs.end_session(version);

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
            jobs,
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

async fn build_executor(
    config: &WorkerConfig,
    exec: Option<Arc<ExecHost>>,
) -> Result<(JobExecutor, JobScorer)> {
    let store = LocalNixStore::connect(config.nix_daemon.max_connections.get())?;
    let evaluator = WorkerEvaluator::new(
        config.fork_workers(),
        config.eval.max_rss.get(),
        config.memory_guard()?,
        config.eval_cache_dir(),
        config.eval.cache_share,
        config.eval.metrics,
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
    )
    .with_exec(exec);
    Ok((executor, JobScorer::new()))
}

fn resume_exec_job(jobs: &WorkerJobs, executor: &JobExecutor, resumed: ResumedJob) {
    let job_id = resumed.stored.job_id.clone();
    let assignment_id = resumed.stored.assignment_id.clone();
    let executor = executor.clone();
    jobs.resume_job(job_id, assignment_id, move |mut updater, abort| {
        Box::pin(async move { executor.resume_exec_job(resumed, &mut updater, abort).await })
    });
}

#[derive(Clone, Copy, Debug)]
enum Side {
    Outbound,
    Inbound,
}

async fn perform_setup(
    conn: &mut ProtoConnection,
    config: &WorkerConfig,
    side: Side,
    running: Vec<RunningAssignment>,
    exec: Option<&ExecHost>,
) -> Result<()> {
    let peer_id = load_or_generate_id(&config.base_dir, config.id.as_deref())
        .context("load or generate persistent worker ID")?;
    if let Some(exec) = exec {
        exec.start_session();
    }
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
        Side::Outbound => {
            perform_handshake(conn, peer_id, config.peer_tokens(), config.capabilities()).await?
        }
    };
    info!(
        ?side,
        negotiated = ?handshake.negotiated,
        version = handshake.version,
        "capabilities negotiated"
    );
    if handshake.version >= RUNNING_JOBS_SINCE {
        conn.send(ClientMessage::RunningJobs {
            assignments: running,
        })
        .await?;
    }
    let exec = exec.filter(|_| handshake.negotiated.exec);
    if handshake.negotiated.build || exec.is_some() {
        let architectures = if config.system.architectures.is_empty() {
            vec![crate::config::host_system()]
        } else {
            config.system.architectures.clone()
        };
        let system_features = if config.system.features.is_empty() {
            crate::nix::system_features::detect().await
        } else {
            config.system.features.clone()
        };
        let host = crate::metrics::host_static();
        let cpu_core_score = config.cpu_core_score();
        info!(
            ?architectures,
            ?system_features,
            max_concurrent_builds = config.build.max_concurrent.get(),
            cpu_count = host.cpu_count,
            ram_total_mb = host.ram_total_mb,
            cpu_core_score,
            "advertising build capabilities"
        );
        conn.send(ClientMessage::WorkerCapabilities {
            architectures,
            system_features,
            max_concurrent_builds: config.build.max_concurrent.get(),
            cpu_count: host.cpu_count,
            ram_total_mb: host.ram_total_mb,
            cpu_core_score,
            zone: config.zone.clone().filter(|z| !z.is_empty()),
            endpoint: config.endpoint.clone().filter(|e| !e.is_empty()),
            build_cores: config.build_cores().map(std::num::NonZeroU32::get),
            exec: exec.map(|exec| Box::new(exec.inventory())),
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
    if exec.is_some() {
        conn.send(ClientMessage::RequestJob {
            kind: JobKind::Exec,
        })
        .await?;
    }
    Ok(())
}

#[cfg(test)]
fn test_executor(gcroots: &std::path::Path) -> JobExecutor {
    let store =
        LocalNixStore::connect_at("/var/empty/gradient-nonexistent.sock", 1).expect("lazy connect");
    let config = WorkerConfig::default();
    JobExecutor::new(
        store.clone(),
        WorkerEvaluator::new(
            std::num::NonZeroUsize::MIN,
            0,
            config.memory_guard().expect("default memory guard"),
            String::new(),
            false,
            false,
        ),
        crate::nix::gcroots::GcRootKeeper::new(
            gcroots.to_str().expect("utf-8 path"),
            Arc::new(store),
        ),
        "ssh".into(),
        crate::executor::log_limit::LogRateLimits {
            burst_bytes_per_min: 0,
            sustained_bytes_per_hour: 0,
        },
        false,
        crate::executor::BuildHost {
            build_cores: None,
            cpu_core_score: 0,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_test_support::prelude::MockProtoServer;
    use gradient_wire::messages::GradientCapabilities;

    fn disconnected_worker(base: &std::path::Path) -> Worker<Disconnected> {
        let config = WorkerConfig {
            base_dir: base.to_owned(),
            ..WorkerConfig::default()
        };
        Worker {
            jobs: Arc::new(WorkerJobs::new(&config)),
            executor: test_executor(base),
            scorer: JobScorer::new(),
            credentials: CredentialStore::new(),
            config,
            conn_state: Disconnected,
            _marker: PhantomData,
        }
    }

    async fn reconnected(
        worker: Worker<Disconnected>,
    ) -> (
        gradient_test_support::prelude::MockServerConn,
        Worker<Connected>,
    ) {
        let server = MockProtoServer::bind().await;
        let (sc, conn) = tokio::join!(
            async {
                let mut sc = server.accept().await;
                sc.handshake(GradientCapabilities::default()).await.unwrap();
                sc
            },
            worker.connect(server.url())
        );
        (sc, worker.into_connected(conn.unwrap()))
    }

    #[tokio::test]
    async fn a_failed_job_report_ends_the_session() {
        let base = tempfile::tempdir().unwrap();
        let worker = disconnected_worker(base.path());
        let jobs = Arc::clone(&worker.jobs);
        let (_sc, worker) = reconnected(worker).await;

        let lose_the_job_lane = async {
            jobs.writer.attached().await;
            jobs.writer.detach();
        };
        let ((_, ended), ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(worker.run(CancellationToken::new()), lose_the_job_lane)
        })
        .await
        .expect("a session that cannot carry job reports ends while the socket stays open");
        ended.unwrap();
    }

    #[tokio::test]
    async fn a_job_finished_while_detached_reports_after_running_jobs() {
        let base = tempfile::tempdir().unwrap();
        let worker = disconnected_worker(base.path());
        let finished = worker.jobs.register(
            "build:1".to_owned(),
            JobKind::Build,
            "dispatch-1".to_owned(),
            None,
        );
        finished
            .done_tx
            .send(("build:1".to_owned(), Ok(())))
            .unwrap();

        let (mut sc, worker) = reconnected(worker).await;

        let script = async {
            let listed = sc
                .recv_until(|msg| match msg {
                    ClientMessage::RunningJobs { assignments } => Some(assignments),
                    ClientMessage::JobCompleted { .. } => panic!("completed before RunningJobs"),
                    _ => None,
                })
                .await
                .unwrap();
            assert_eq!(
                listed,
                vec![RunningAssignment {
                    assignment_id: "dispatch-1".into(),
                    kind: JobKind::Build,
                }]
            );
            let completed = sc
                .recv_until(|msg| match msg {
                    ClientMessage::JobCompleted { job_id, .. } => Some(job_id),
                    _ => None,
                })
                .await
                .unwrap();
            assert_eq!(completed, "build:1");
            sc.close().await;
        };
        let ((_, ended), ()) = tokio::join!(worker.run(CancellationToken::new()), script);
        ended.unwrap();
    }
}
