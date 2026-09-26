/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod mock_server;

pub use mock_server::*;

use tokio::net::TcpListener;

use crate::session::frame::{ProtoSocket, accept_tungstenite};

/// An authority-side and a peer-side socket connected over loopback.
pub async fn loopback() -> (ProtoSocket, ProtoSocket) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let url = format!("ws://{}", listener.local_addr().expect("local addr"));
    let accept = async {
        let (tcp, _) = listener.accept().await.expect("accept");
        let ws = tokio_tungstenite::accept_async(tokio_tungstenite::MaybeTlsStream::Plain(tcp))
            .await
            .expect("ws handshake");
        accept_tungstenite(ws)
    };
    let (authority, peer) = tokio::join!(accept, crate::client::dial(&url));
    (authority, peer.expect("dial loopback"))
}
