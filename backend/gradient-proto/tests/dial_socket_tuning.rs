/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Every proto connection is opened through `client::dial`, so the frame
//! ceiling and the socket tuning are asserted once, here, rather than at each
//! call site.

use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use gradient_proto::client::dial;
use gradient_proto::handler::MAX_PROTO_MESSAGE_SIZE;
use gradient_proto::session::frame::ProtoSocket;

/// Dial a listener that upgrades exactly one inbound connection. The upgrade
/// has to be driven concurrently with the dial or neither handshake completes,
/// and the server half is returned so the socket outlives the assertions.
async fn dial_one_shot() -> (
    WebSocketStream<MaybeTlsStream<TcpStream>>,
    WebSocketStream<MaybeTlsStream<TcpStream>>,
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

    let client = match client.expect("dial") {
        ProtoSocket::Tungstenite(ws) => *ws,
        ProtoSocket::Axum(_) => panic!("dial must produce a tungstenite socket"),
    };
    (server, client)
}

#[tokio::test]
async fn dial_disables_nagle_on_the_connection() {
    let (_server, client) = dial_one_shot().await;

    assert!(
        client
            .get_ref()
            .get_ref()
            .nodelay()
            .expect("read nodelay on the dialed socket")
    );
}

#[tokio::test]
async fn dial_applies_the_proto_frame_ceiling() {
    let (_server, client) = dial_one_shot().await;

    let config = client.get_config();
    assert_eq!(config.max_message_size, Some(MAX_PROTO_MESSAGE_SIZE));
    assert_eq!(config.max_frame_size, Some(MAX_PROTO_MESSAGE_SIZE));
}
