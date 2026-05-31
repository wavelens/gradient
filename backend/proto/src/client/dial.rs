/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http;

use crate::session::frame::ProtoSocket;

/// Open a WebSocket connection to `url` and wrap it in the unified
/// `ProtoSocket` type. The caller then runs the handshake of their choice
/// (`session::handshake::as_peer` or `as_authority`) on the returned socket.
pub async fn dial(url: &str) -> Result<ProtoSocket> {
    let (ws, _resp) = connect_async(url)
        .await
        .with_context(|| format!("dial WebSocket {url}"))?;
    Ok(ProtoSocket::Tungstenite(Box::new(ws)))
}

/// Like [`dial`] but attaches an `Authorization: GRAD<key>` header when an
/// `api_key` is supplied, used to authenticate against a remote cache's
/// read-only `/cache/{cache}/proto` endpoint.
pub async fn dial_with_auth(url: &str, api_key: Option<&str>) -> Result<ProtoSocket> {
    let Some(key) = api_key else {
        return dial(url).await;
    };

    let mut request = url
        .into_client_request()
        .with_context(|| format!("build WebSocket request {url}"))?;
    request.headers_mut().insert(
        http::header::AUTHORIZATION,
        format!("GRAD{key}")
            .parse()
            .context("encode Authorization header")?,
    );

    let (ws, _resp) = connect_async(request)
        .await
        .with_context(|| format!("dial WebSocket {url}"))?;
    Ok(ProtoSocket::Tungstenite(Box::new(ws)))
}
