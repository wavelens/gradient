/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! TCP tuning shared by every endpoint that carries the protocol.

use tokio::net::TcpStream;
use tracing::warn;

/// Disable Nagle's algorithm on `stream`.
///
/// One connection interleaves small latency-critical control frames with bulk
/// transfers. Nagle holds a small write back until the previous segment is
/// acknowledged, which pairs with the peer's delayed ACK to add tens of
/// milliseconds to exactly the frames an RPC is blocked on. A socket that
/// refuses the option still works, just slower, so failure is logged and
/// ignored rather than propagated.
pub fn disable_nagle(stream: &TcpStream) {
    if let Err(e) = stream.set_nodelay(true) {
        warn!(error = %e, "failed to set TCP_NODELAY");
    }
}
