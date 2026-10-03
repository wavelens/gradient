/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod auth;
mod build_request;
mod build_wait;
mod commands;
mod daemon_build;
mod exec;
mod host_key;
mod ingest;
mod listener;
mod nar;
mod roots;
mod server;
mod session;
mod store;

use gradient_core::ServerState;
use russh::server::Server as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const ROOTS_TTL: Duration = Duration::from_secs(3600);

pub async fn start(
    state: Arc<ServerState>,
    scheduler: Arc<gradient_scheduler::Scheduler>,
) -> std::io::Result<()> {
    let ssh = &state.config.ssh;
    let host = ssh
        .listen_address
        .clone()
        .unwrap_or_else(|| state.config.server.listen_addr.clone());
    let key = host_key::load_or_generate(
        ssh.host_key_file.as_deref(),
        Path::new(&state.config.server.base_dir),
    )
    .await
    .map_err(std::io::Error::other)?;
    let listener = listener::bind(&host, ssh.port).await?;
    tracing::info!(%host, port = ssh.port, "ssh server listening");

    let config = Arc::new(server::config(key));
    let roots = Arc::new(roots::Roots::new(ROOTS_TTL));
    let mut server = server::SshServer::new(state.clone(), roots, scheduler);
    let token = state.shutdown.token();
    state.shutdown.spawn(async move {
        let running = server.run_on_socket(config, &listener);
        let handle = running.handle();
        tokio::select! {
            result = running => {
                if let Err(error) = result {
                    tracing::error!(%error, "ssh server stopped");
                }
            }
            () = token.cancelled() => handle.shutdown("server shutting down".into()),
        }
    });

    Ok(())
}
