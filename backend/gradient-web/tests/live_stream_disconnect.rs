/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![allow(clippy::disallowed_methods, reason = "test harness server")]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::WebSocketUpgrade;
use axum::response::Response;
use axum::routing::get;
use gradient_types::EventBus;
use gradient_util::shutdown::Shutdown;
use tokio::net::TcpListener;

type Done = Arc<tokio::sync::Notify>;

async fn live_route(
    State((tx, done, shutdown)): State<(EventBus, Done, Shutdown)>,
    ws: WebSocketUpgrade,
) -> Response {
    let rx = tx.subscribe();
    ws.on_upgrade(move |socket| async move {
        gradient_web::endpoints::live::live_stream(
            socket,
            rx,
            |env| Some(env.to_line()),
            gradient_web::endpoints::live::skip_lag,
            shutdown.token(),
        )
        .await;
        done.notify_one();
    })
}

#[tokio::test]
async fn live_stream_ends_when_the_client_disconnects() {
    let tx = EventBus::new(16);
    let done: Done = Arc::new(tokio::sync::Notify::new());
    let shutdown = Shutdown::new();

    let app =
        Router::new()
            .route("/live", get(live_route))
            .with_state((tx, Arc::clone(&done), shutdown));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let (client, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/live"))
        .await
        .expect("client connects");

    // The client is leaving without the channel ever publishing an event. A write-only loop cannot
    // detect this case.
    drop(client);

    tokio::time::timeout(Duration::from_secs(5), done.notified())
        .await
        .expect("stream task must end when the client disconnects");
}

#[tokio::test]
async fn live_stream_ends_when_the_server_shuts_down() {
    let tx = EventBus::new(16);
    let done: Done = Arc::new(tokio::sync::Notify::new());
    let shutdown = Shutdown::new();

    let app = Router::new().route("/live", get(live_route)).with_state((
        tx,
        Arc::clone(&done),
        shutdown.clone(),
    ));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let (_client, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/live"))
        .await
        .expect("client connects");

    shutdown.cancel();

    tokio::time::timeout(Duration::from_secs(5), done.notified())
        .await
        .expect("stream task must end on shutdown");
}
