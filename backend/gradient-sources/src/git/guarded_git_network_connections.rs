/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::git_network_limits::STALLED_SERVER_TIMEOUT;
use gradient_util::http::HttpClientForUserSuppliedUrls;
use gradient_util::http_validation::{DestinationUrlRejectedError, UserUrlDestinationAllowlist};
use std::future::Future;
use std::io;
use std::sync::{LazyLock, PoisonError, RwLock};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;

static GIT_NETWORK_IO_TWO_WORKER_TOKIO_RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("gradient-git-network")
        .enable_all()
        .build()
        .expect("Build a tokio runtime for network connections to Git.")
});

static GIT_HTTP_CLIENT_RWLOCK: RwLock<Option<HttpClientForUserSuppliedUrls>> = RwLock::new(None);

static PUBLIC_ONLY_HTTP_CLIENT: LazyLock<HttpClientForUserSuppliedUrls> = LazyLock::new(|| {
    HttpClientForUserSuppliedUrls::new(UserUrlDestinationAllowlist::default())
        .expect("Build an HTTP Client with a Guard for Public Addresses Only")
});

#[derive(Debug, thiserror::Error)]
pub(crate) enum ConnectError {
    #[error(transparent)]
    DestinationRejectedByAdminAllowlist(#[from] DestinationUrlRejectedError),
    #[error(transparent)]
    NoAllowedAddressAcceptedTcpConnection(io::Error),
}

pub(crate) fn block_on_git_network_runtime_from_blocking_thread<F: Future>(future: F) -> F::Output {
    GIT_NETWORK_IO_TWO_WORKER_TOKIO_RUNTIME.block_on(future)
}

pub(crate) async fn connect_to_allowed_git_server_addresses_with_timeout(
    host: &str,
    port: u16,
    client: &HttpClientForUserSuppliedUrls,
) -> Result<TcpStream, ConnectError> {
    let addresses = client
        .allowlist()
        .allowed_socket_addresses_for_host_and_port(host, port)
        .await?;
    let mut last_error = None;
    for address in addresses {
        match tokio::time::timeout(STALLED_SERVER_TIMEOUT, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => last_error = Some(error),
            Err(_) => {
                last_error = Some(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "Connection to {address} timed out after {} seconds",
                        STALLED_SERVER_TIMEOUT.as_secs()
                    ),
                ))
            }
        }
    }
    Err(ConnectError::NoAllowedAddressAcceptedTcpConnection(
        last_error.unwrap_or_else(|| io::ErrorKind::AddrNotAvailable.into()),
    ))
}

pub fn set_default_git_http_client(client: HttpClientForUserSuppliedUrls) {
    *GIT_HTTP_CLIENT_RWLOCK
        .write()
        .unwrap_or_else(PoisonError::into_inner) = Some(client);
}

pub(super) fn clone_git_http_client_or_public_client() -> HttpClientForUserSuppliedUrls {
    GIT_HTTP_CLIENT_RWLOCK
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .unwrap_or_else(|| PUBLIC_ONLY_HTTP_CLIENT.clone())
}

#[cfg(test)]
pub(crate) fn set_guarded_git_client_to_allow_loopback_for_tests() {
    let allowlist =
        UserUrlDestinationAllowlist::parse(&["127.0.0.0/8".to_owned(), "::1/128".to_owned()])
            .expect("Build a guarded HTTP client for tests with loopback addresses allowed.");
    set_default_git_http_client(
        HttpClientForUserSuppliedUrls::new(allowlist)
            .expect("Build a guarded HTTP client for tests with loopback addresses allowed."),
    );
}
