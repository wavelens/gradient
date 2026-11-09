// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

#![expect(
    clippy::unwrap_used,
    clippy::result_large_err,
    reason = "test scaffolding: set the websocket callback to tokio-tungstenite's handshake signature, large error type included. Make a server that can't start fail the test clearly."
)]

use futures::SinkExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

pub async fn start_local_websocket_server_with_text_frames_returning_http_base_url(
    frames: Vec<String>,
) -> (String, oneshot::Receiver<(String, Option<String>)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (request_tx, request_rx) = oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket =
            tokio_tungstenite::accept_hdr_async(stream, |request: &Request, response: Response| {
                let authorization = request
                    .headers()
                    .get("Authorization")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let _ = request_tx.send((request.uri().path().to_owned(), authorization));
                Ok(response)
            })
            .await
            .unwrap();
        for frame in frames {
            socket.send(Message::Text(frame.into())).await.unwrap();
        }
        socket.close(None).await.unwrap();
    });
    (format!("http://{address}"), request_rx)
}
