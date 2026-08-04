/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A `/live` channel must let go of a client that walked away.
//!
//! The stream only ever wrote to its socket, so a closed browser tab was
//! noticed only if some later event happened to fail the send. A channel that
//! stays quiet - a finished project, an idle cache - therefore kept its task
//! and its file descriptor forever, and the server accumulated CLOSE-WAIT
//! sockets (258 of them over a few hours in production) until it would have
//! run out of descriptors.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::WebSocketUpgrade;
use axum::response::Response;
use axum::routing::get;
use gradient_types::BoardEvent;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

/// Signals when the server-side stream task has returned.
type Done = Arc<tokio::sync::Notify>;

async fn live_route(
    State((tx, done)): State<(broadcast::Sender<BoardEvent>, Done)>,
    ws: WebSocketUpgrade,
) -> Response {
    let rx = tx.subscribe();
    ws.on_upgrade(move |socket| async move {
        gradient_web::endpoints::live::live_stream(socket, rx, |ev| serde_json::to_string(ev).ok())
            .await;
        done.notify_one();
    })
}

#[tokio::test]
async fn live_stream_ends_when_the_client_disconnects() {
    let (tx, _rx) = broadcast::channel(16);
    let done: Done = Arc::new(tokio::sync::Notify::new());

    let app = Router::new()
        .route("/live", get(live_route))
        .with_state((tx, Arc::clone(&done)));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let (client, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/live"))
        .await
        .expect("client connects");

    // The client goes away without the channel ever publishing an event -
    // exactly the case a write-only loop cannot detect.
    drop(client);

    tokio::time::timeout(Duration::from_secs(5), done.notified())
        .await
        .expect("stream task must end when the client disconnects");
}
