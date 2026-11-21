/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::allowlisted_git_smart_http_subtransport::create_smart_transport_for_http_or_https_remote;
use super::git_credentials::{
    CURRENT_THREAD_GIT_ALLOWLIST_REFUSAL_TEXT, CURRENT_THREAD_GIT_CREDENTIALS,
};
use super::git_network_limits::{
    GitNetworkStreamReadDeadlineAndByteLimitChecker, RefAdvertisementEnd, STALLED_SERVER_TIMEOUT,
};
use super::guarded_git_network_connections::{
    ConnectError, block_on_git_network_runtime_from_blocking_thread,
    connect_to_allowed_git_server_addresses_with_timeout,
};
use super::ssh;
use super::url::{GIT_PROTOCOL_DEFAULT_PORT, parse_git_protocol_url};
use git2::transport::{Service, SmartSubtransport, SmartSubtransportStream, Transport};
use gradient_util::http::HttpClientForUserSuppliedUrls;
use std::io::{self, Read, Write};
use std::sync::Once;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

type TransportFromRemoteFunction = fn(&git2::Remote<'_>) -> Result<Transport, git2::Error>;

pub fn register_git_transports_once() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        let ssh_transports =
            ssh::SCHEMES.map(|scheme| (scheme, ssh::transport as TransportFromRemoteFunction));
        let git_and_http_transports = [
            (
                "git",
                create_smart_transport_for_git_remote as TransportFromRemoteFunction,
            ),
            (
                "http",
                create_smart_transport_for_http_or_https_remote as TransportFromRemoteFunction,
            ),
            (
                "https",
                create_smart_transport_for_http_or_https_remote as TransportFromRemoteFunction,
            ),
        ];
        for (scheme, factory) in ssh_transports.into_iter().chain(git_and_http_transports) {
            // SAFETY: All registrations run exactly once through `Once` before this crate can create a transport.
            if let Err(error) = unsafe { git2::transport::register(scheme, factory) } {
                tracing::error!(%error, scheme, "failed to register the Git transport");
            }
        }
    });
}

fn create_smart_transport_for_git_remote(
    remote: &git2::Remote<'_>,
) -> Result<Transport, git2::Error> {
    let client =
        CURRENT_THREAD_GIT_CREDENTIALS.with_borrow(|credentials| credentials.http_client().clone());
    Transport::smart(remote, false, GitProtocolV0TcpSubtransport { client })
}

struct GitProtocolV0TcpSubtransport {
    client: HttpClientForUserSuppliedUrls,
}

impl SmartSubtransport for GitProtocolV0TcpSubtransport {
    fn action(
        &self,
        url: &str,
        action: Service,
    ) -> Result<Box<dyn SmartSubtransportStream>, git2::Error> {
        let program = match action {
            Service::UploadPackLs => "git-upload-pack",
            Service::ReceivePackLs => "git-receive-pack",
            Service::UploadPack | Service::ReceivePack => {
                return Err(git2::Error::from_str("git:// stream requested twice"));
            }
        };
        let url = parse_git_protocol_url(url).map_err(|e| git2::Error::from_str(&e.to_string()))?;
        let host = url.git_server_host_name();
        let port = url.git_server_port().unwrap_or(GIT_PROTOCOL_DEFAULT_PORT);
        let stream = block_on_git_network_runtime_from_blocking_thread(
            connect_to_allowed_git_server_addresses_with_timeout(host, port, &self.client),
        )
        .map_err(|error| {
            if let ConnectError::DestinationRejectedByAdminAllowlist(refusal) = &error {
                CURRENT_THREAD_GIT_ALLOWLIST_REFUSAL_TEXT.set(Some(refusal.to_string()));
            }
            git2::Error::from_str(&error.to_string())
        })?;

        let request = format!("{program} /{}\0host={host}\0", url.repository_path());
        let packet = format!("{:04x}{request}", request.len() + 4);
        let mut stream = GitNetworkStreamReadDeadlineAndByteLimitChecker::new(
            GitProtocolTcpStreamWithStallTimeout(stream),
            RefAdvertisementEnd::AdvertisementEndsAtFirstFlushPacket,
        );
        stream
            .write_all(packet.as_bytes())
            .map_err(|e| git2::Error::from_str(&e.to_string()))?;
        Ok(Box::new(stream))
    }

    fn close(&self) -> Result<(), git2::Error> {
        Ok(())
    }
}

struct GitProtocolTcpStreamWithStallTimeout(TcpStream);

impl Read for GitProtocolTcpStreamWithStallTimeout {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        block_on_git_network_runtime_from_blocking_thread(async {
            tokio::time::timeout(STALLED_SERVER_TIMEOUT, self.0.read(buf)).await
        })
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "Git server sent nothing for {} seconds",
                    STALLED_SERVER_TIMEOUT.as_secs()
                ),
            )
        })?
    }
}

impl Write for GitProtocolTcpStreamWithStallTimeout {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        block_on_git_network_runtime_from_blocking_thread(async {
            tokio::time::timeout(STALLED_SERVER_TIMEOUT, self.0.write(buf)).await
        })
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "Git server accepted no data for {} seconds",
                    STALLED_SERVER_TIMEOUT.as_secs()
                ),
            )
        })?
    }

    fn flush(&mut self) -> io::Result<()> {
        block_on_git_network_runtime_from_blocking_thread(self.0.flush())
    }
}
