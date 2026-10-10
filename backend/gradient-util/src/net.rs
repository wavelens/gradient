/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use tokio::net::TcpStream;
use tracing::warn;

/// One connection is interleaving small control frames with bulk transfers. Nagle and the peer's
/// delayed ACK are adding tens of milliseconds to exactly the frames an RPC is waiting on. A socket
/// refusing the option is still working, only slower.
pub fn disable_nagle(stream: &TcpStream) {
    if let Err(e) = stream.set_nodelay(true) {
        warn!(error = %e, "failed to set TCP_NODELAY");
    }
}

/// A lost network closes nothing. The kernel then keeps the connection for about fifteen minutes,
/// and the worker keeps waiting on it. The server drops a silent worker after the same two minutes.
pub const UNACKNOWLEDGED_DATA_LIMIT: Duration = Duration::from_secs(120);

pub fn limit_unacknowledged_data(stream: &TcpStream) {
    #[cfg(target_os = "linux")]
    if let Err(e) =
        socket2::SockRef::from(stream).set_tcp_user_timeout(Some(UNACKNOWLEDGED_DATA_LIMIT))
    {
        warn!(error = %e, "failed to set TCP_USER_TIMEOUT");
    }
    #[cfg(not(target_os = "linux"))]
    let _ = stream;
}
