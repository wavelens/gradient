/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::session::frame::{MAX_PROTO_MESSAGE_SIZE, ProtoSocket};

/// Open a WebSocket connection to `url` and wrap it in the unified
/// `ProtoSocket` type. The caller then runs the handshake of their choice
/// (`session::handshake::as_peer` or `as_authority`) on the returned socket.
pub async fn dial(url: &str) -> Result<ProtoSocket> {
    let request = url
        .into_client_request()
        .with_context(|| format!("build WebSocket request {url}"))?;
    connect(request, url).await
}

/// Like [`dial`] but attaches an `Authorization: Bearer GRAD<key>` header when
/// an `api_key` is supplied, used to authenticate against a remote cache's
/// read-only `/cache/{cache}/proto` endpoint. The server only accepts API keys
/// via the `Bearer` scheme, so the `GRAD` token must be wrapped in it.
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

/// The one place a proto WebSocket is opened. Every dial carries the same
/// frame ceiling as the inbound upgrade (issue #110) and the same socket
/// tuning, so a peer's limits never depend on which side placed the call.
async fn connect(request: http::Request<()>, url: &str) -> Result<ProtoSocket> {
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_PROTO_MESSAGE_SIZE))
        .max_frame_size(Some(MAX_PROTO_MESSAGE_SIZE));

    // `disable_nagle` is tungstenite's name for `set_nodelay(true)`; see
    // `gradient_util::net::disable_nagle` for why every proto socket wants it.
    let (ws, _resp) = connect_async_with_config(request, Some(config), true)
        .await
        .with_context(|| format!("dial WebSocket {url}"))?;

    Ok(ProtoSocket::Tungstenite(Box::new(ws)))
}
