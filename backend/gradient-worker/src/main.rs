/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod config;
mod connection_state;
mod executor;
mod listener;
mod metrics;
mod nix;
mod proto;
mod shutdown;
mod traits;
mod worker;
mod worker_pool;

use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use tokio_util::task::TaskTracker;
use tracing::{error, info, warn};

use config::WorkerConfig;
use gradient_util::logging::{LogSetup, LogWriter, TraceSetup};
use gradient_worker_client::reconnect::{
    RunOutcome, SessionEnd, backoff_after_session, retry_reconnect,
};
use shutdown::Shutdown;
use worker::Worker;

const MAX_BACKOFF: Duration = Duration::from_secs(60);
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);

fn main() -> Result<()> {
    let config = WorkerConfig::parse();

    // Logs are going to stderr because eval-worker subprocesses are using stdout for rkyv frames. A
    // tracing line on stdout would corrupt the frame stream.
    let overrides = log_overrides(&config);
    gradient_util::logging::init(&LogSetup {
        level: &config.log.level_default,
        overrides: &overrides,
        quiet: &[],
        honor_rust_log: false,
        writer: LogWriter::Stderr,
        trace: config.log.trace_dir.as_deref().map(|dir| TraceSetup {
            dir,
            process: if config.eval_subprocess {
                "eval"
            } else {
                "worker"
            },
        }),
    });

    if config.eval_subprocess {
        return nix::eval_worker::run_eval_worker().map_err(anyhow::Error::from);
    }

    if let Some(requests_path) = config.eval_driver.clone() {
        let rt = tokio::runtime::Runtime::new()?;
        let code = rt.block_on(worker_pool::driver::run_eval_driver(
            &requests_path,
            &config.eval_cache_dir(),
        ))?;
        std::process::exit(code);
    }

    // The crypto provider must be installed before the first TLS handshake. rustls 0.23 is
    // panicking without one (#232).
    gradient_util::http::init_crypto_provider();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        info!(server_url = %config.server_url, "gradient-worker starting");

        if !config.server_url.contains("proto") {
            warn!(
                server_url = %config.server_url,
                "server URL does not contain 'proto'; expected the server's /proto WebSocket endpoint (e.g. wss://gradient.example.com/proto)"
            );
        }

        let shutdown = Shutdown::new();
        install_signal_handler(shutdown.clone(), drain_budget(&config));

        let sessions = TaskTracker::new();

        if config.nar.partial_ttl_secs > 0 {
            let gc_config = config.clone();
            let gc_shutdown = shutdown.clone();
            #[expect(clippy::disallowed_methods, reason = "returns on drain_requested")]
            tokio::spawn(async move {
                let ttl = std::time::Duration::from_secs(gc_config.nar.partial_ttl_secs);
                let store = match gradient_storage::PartialStore::new(gc_config.nar_partial_dir()) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(error = %e, "NAR partial GC: open failed");
                        return;
                    }
                };
                let period =
                    std::time::Duration::from_secs((gc_config.nar.partial_ttl_secs / 4).clamp(60, 3600));
                let mut tick = tokio::time::interval(period);
                loop {
                    tokio::select! {
                        _ = gc_shutdown.drain_requested() => return,
                        _ = tick.tick() => {}
                    }
                    match store.gc(ttl).await {
                        Ok(n) if n > 0 => info!(removed = n, "swept stale NAR partials"),
                        Ok(_) => {}
                        Err(e) => warn!(error = %e, "NAR partial GC failed"),
                    }
                }
            });
        }

        if config.discoverable {
            let listener_config = config.clone();
            let listener_shutdown = shutdown.clone();
            let listener_sessions = sessions.clone();
            #[expect(clippy::disallowed_methods, reason = "returns on drain_requested")]
            tokio::spawn(async move {
                if let Err(e) = listener::start_listener(
                    listener_config,
                    listener_shutdown,
                    listener_sessions,
                )
                .await
                {
                    error!(error = %e, "listener failed");
                }
            });
        }

        sd_notify::notify(&[sd_notify::NotifyState::Ready])?;
        let mut backoff = INITIAL_BACKOFF;

        let initial = tokio::select! {
            _ = shutdown.drain_requested() => None,
            w = async {
                loop {
                    match Worker::connect(config.clone()).await {
                        Ok(w) => break w,
                        Err(e) => {
                            error!(
                                error = %e,
                                delay_secs = backoff.as_secs(),
                                "connection failed; retrying"
                            );
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(MAX_BACKOFF);
                        }
                    }
                }
            } => Some(w),
        };
        let Some(mut worker) = initial else {
            info!("shutdown requested during initial connect");
            drain_sessions(&sessions, &shutdown).await;
            return Ok(());
        };
        backoff = INITIAL_BACKOFF;

        let executor_handle = worker.executor_handle();

        loop {
            let (disconnected, outcome) = worker.run(shutdown.clone()).await;

            if shutdown.is_stopping() {
                info!("worker drained; tearing down");
                drop(disconnected);
                drain_sessions(&sessions, &shutdown).await;
                executor_handle.shutdown().await;
                return Ok(());
            }

            let session_end = match outcome {
                Ok(outcome) => {
                    match outcome {
                        RunOutcome::Drained => warn!(
                            delay_secs = backoff.as_secs(),
                            "server drained the session; reconnecting until it is back"
                        ),
                        RunOutcome::Refused => warn!(
                            delay_secs = backoff.as_secs(),
                            "server refused the session; backing off"
                        ),
                        RunOutcome::CleanDisconnect => {
                            warn!(delay_secs = backoff.as_secs(), "connection closed; reconnecting")
                        }
                    }
                    SessionEnd::from(outcome)
                }
                Err(e) => {
                    error!(error = %e, delay_secs = backoff.as_secs(), "dispatch loop error; reconnecting");
                    SessionEnd::Served
                }
            };

            let reconnected = tokio::select! {
                _ = shutdown.drain_requested() => None,
                conn = retry_reconnect(
                    async || disconnected.reconnect().await,
                    |delay| tokio::time::sleep(delay),
                    backoff,
                    MAX_BACKOFF,
                ) => Some(conn),
            };
            match reconnected {
                Some(conn) => {
                    info!("reconnected successfully");
                    worker = disconnected.into_connected(conn);
                    backoff =
                        backoff_after_session(backoff, session_end, INITIAL_BACKOFF, MAX_BACKOFF);
                }
                None => {
                    info!("shutdown requested during reconnect");
                    drain_sessions(&sessions, &shutdown).await;
                    executor_handle.shutdown().await;
                    return Ok(());
                }
            }
        }
    })
}

fn install_signal_handler(shutdown: Shutdown, budget: Option<Duration>) {
    #[expect(
        clippy::disallowed_methods,
        reason = "outlives every session; the second signal or the budget ends it"
    )]
    tokio::spawn(async move {
        crate::shutdown::stop_sequence(&shutdown, budget, next_stop_signal).await
    });
}

fn drain_budget(config: &WorkerConfig) -> Option<Duration> {
    (config.drain_timeout_secs > 0).then(|| Duration::from_secs(config.drain_timeout_secs))
}

async fn drain_sessions(sessions: &TaskTracker, shutdown: &Shutdown) {
    sessions.close();
    if sessions.is_empty() {
        return;
    }

    info!(
        sessions = sessions.len(),
        "waiting for inbound sessions to drain"
    );
    tokio::select! {
        () = sessions.wait() => {}
        () = shutdown.abort_requested() => {
            warn!(sessions = sessions.len(), "inbound sessions still draining; stopping anyway")
        }
    }
}

async fn next_stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => tokio::select! {
                _ = tokio::signal::ctrl_c() => info!("received SIGINT"),
                _ = sigterm.recv() => info!("received SIGTERM"),
            },
            Err(e) => {
                error!(error = %e, "failed to install SIGTERM handler; SIGINT only");
                let _ = tokio::signal::ctrl_c().await;
                info!("received SIGINT");
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        info!("received Ctrl-C");
    }
}

fn log_overrides(config: &WorkerConfig) -> Vec<(&'static str, Option<&str>)> {
    const EVAL_TARGETS: &[&str] = &[
        "gradient_worker::nix",
        "gradient_worker::worker_pool",
        "gradient_worker::executor::eval",
    ];
    const BUILD_TARGETS: &[&str] = &[
        "gradient_worker::executor::build",
        "gradient_worker::executor::compress",
    ];
    const PROTO_TARGETS: &[&str] = &[
        "gradient_worker::proto",
        "gradient_worker::connection_state",
        "gradient_worker::listener",
        "gradient_worker_client",
        "gradient_wire",
    ];

    fn area<'a>(
        targets: &'static [&'static str],
        level: Option<&'a str>,
    ) -> impl Iterator<Item = (&'static str, Option<&'a str>)> {
        targets.iter().map(move |t| (*t, level))
    }

    area(EVAL_TARGETS, config.log.level_eval.as_deref())
        .chain(area(BUILD_TARGETS, config.log.level_build.as_deref()))
        .chain(area(PROTO_TARGETS, config.log.level_proto.as_deref()))
        .collect()
}
