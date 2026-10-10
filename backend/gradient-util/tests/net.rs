/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use tokio::net::{TcpListener, TcpStream};

use gradient_util::net::{UNACKNOWLEDGED_DATA_LIMIT, disable_nagle, limit_unacknowledged_data};

#[tokio::test]
async fn disable_nagle_sets_tcp_nodelay_on_a_live_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let (client, accepted) = tokio::join!(TcpStream::connect(addr), listener.accept());
    let client = client.expect("connect");
    let _accepted = accepted.expect("accept");

    assert!(
        !client.nodelay().expect("read nodelay"),
        "a fresh socket must start Nagle-enabled, else this asserts nothing"
    );

    disable_nagle(&client);

    assert!(client.nodelay().expect("read nodelay"));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn limit_unacknowledged_data_sets_tcp_user_timeout_on_a_live_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let (client, accepted) = tokio::join!(TcpStream::connect(addr), listener.accept());
    let client = client.expect("connect");
    let _accepted = accepted.expect("accept");
    let limit = || {
        socket2::SockRef::from(&client)
            .tcp_user_timeout()
            .expect("read TCP_USER_TIMEOUT")
    };

    assert_eq!(
        limit(),
        None,
        "a fresh socket must start without a limit, else this asserts nothing"
    );

    limit_unacknowledged_data(&client);

    assert_eq!(limit(), Some(UNACKNOWLEDGED_DATA_LIMIT));
}
