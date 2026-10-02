/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

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
