/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::net::SocketAddr;

use anyhow::{Context, Result};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{client_async_tls_with_config, connect_async_with_config};

use crate::session::frame::{BULK_CHUNK_SIZE, MAX_PROTO_MESSAGE_SIZE, ProtoSocket};

pub async fn dial(url: &str) -> Result<ProtoSocket> {
    let request = url
        .into_client_request()
        .with_context(|| format!("build WebSocket request {url}"))?;
    connect(request, url).await
}

/// The server is accepting API keys only through the `Bearer` scheme. The `GRAD` token must be
/// wrapped in it.
pub async fn dial_with_auth(url: &str, api_key: Option<&str>) -> Result<ProtoSocket> {
    let Some(key) = api_key else {
        return dial(url).await;
    };

    let mut request = url
        .into_client_request()
        .with_context(|| format!("build WebSocket request {url}"))?;
    request.headers_mut().insert(
        http::header::AUTHORIZATION,
        format!("Bearer GRAD{key}")
            .parse()
            .context("encode Authorization header")?,
    );

    connect(request, url).await
}

pub async fn dial_with_prechecked_socket_addrs(
    url: &str,
    addrs: &[SocketAddr],
) -> Result<ProtoSocket> {
    let request = url
        .into_client_request()
        .with_context(|| format!("build WebSocket request {url}"))?;
    let tcp = TcpStream::connect(addrs)
        .await
        .with_context(|| format!("connect to {url}"))?;
    tcp.set_nodelay(true).context("disable Nagle")?;

    let (ws, _resp) = client_async_tls_with_config(request, tcp, Some(proto_config()), None)
        .await
        .with_context(|| format!("dial WebSocket {url}"))?;

    gradient_util::net::limit_unacknowledged_data(ws.get_ref().get_ref());

    Ok(ProtoSocket::tungstenite(ws))
}

/// Every dial is carrying the same frame ceiling as the inbound upgrade (issue #110) and the same
/// socket tuning. A peer's limits must never depend on which side placed the call.
fn proto_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_PROTO_MESSAGE_SIZE))
        .max_frame_size(Some(MAX_PROTO_MESSAGE_SIZE))
        .read_buffer_size(BULK_CHUNK_SIZE)
}

async fn connect(request: http::Request<()>, url: &str) -> Result<ProtoSocket> {
    let (ws, _resp) = connect_async_with_config(request, Some(proto_config()), true)
        .await
        .with_context(|| format!("dial WebSocket {url}"))?;

    gradient_util::net::limit_unacknowledged_data(ws.get_ref().get_ref());

    Ok(ProtoSocket::tungstenite(ws))
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

    use super::dial;
    use crate::session::frame::{MAX_PROTO_MESSAGE_SIZE, ProtoSocket};

    async fn dial_one_shot() -> (
        WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
        ProtoSocket,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");

        let serve = async {
            let (tcp, _) = listener.accept().await.expect("accept");
            tokio_tungstenite::accept_async(MaybeTlsStream::Plain(tcp))
                .await
                .expect("server-side upgrade")
        };

        let url = format!("ws://{addr}/proto");
        let (server, client) = tokio::join!(serve, dial(&url));
        (server, client.expect("dial"))
    }

    #[tokio::test]
    async fn dial_disables_nagle_on_the_connection() {
        let (_server, client) = dial_one_shot().await;
        let stream = client
            .tungstenite_stream()
            .expect("dial produces a tungstenite socket");

        assert!(
            stream
                .get_ref()
                .get_ref()
                .nodelay()
                .expect("read nodelay on the dialed socket")
        );
    }

    #[tokio::test]
    async fn dial_applies_the_proto_frame_ceiling() {
        let (_server, client) = dial_one_shot().await;
        let stream = client
            .tungstenite_stream()
            .expect("dial produces a tungstenite socket");

        let config = stream.get_config();
        assert_eq!(config.max_message_size, Some(MAX_PROTO_MESSAGE_SIZE));
        assert_eq!(config.max_frame_size, Some(MAX_PROTO_MESSAGE_SIZE));
    }
}
