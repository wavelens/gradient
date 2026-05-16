/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use tokio_tungstenite::connect_async;

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
