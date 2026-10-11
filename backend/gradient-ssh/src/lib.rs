/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod auth;
mod basic_derivation;
mod build_log;
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
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

const ROOTS_TTL: Duration = Duration::from_secs(3600);

pub async fn start(state: Arc<ServerState>) -> std::io::Result<()> {
    let ssh = &state.config.ssh;
    let key =
        host_key::load_or_generate(ssh.host_key_file.as_deref(), &state.config.server.base_dir)
            .await
            .map_err(std::io::Error::other)?;
    let config = Arc::new(server::config(key));
    let roots = Arc::new(roots::Roots::new(ROOTS_TTL));
    for host in &ssh.listen_addresses {
        let listener = listener::bind(host, ssh.port).await?;
        tracing::info!(%host, port = ssh.port, "ssh server listening");
        accept_on(&state, listener, config.clone(), roots.clone());
    }

    Ok(())
}

fn accept_on(
    state: &Arc<ServerState>,
    listener: TcpListener,
    config: Arc<russh::server::Config>,
    roots: Arc<roots::Roots>,
) {
    let mut server = server::SshServer::new(state.clone(), roots);
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
}
