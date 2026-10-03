/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use socket2::{Domain, Socket, Type};
use std::io;
use tokio::net::TcpListener;

const BACKLOG: i32 = 1024;

pub(crate) async fn bind(host: &str, port: u16) -> io::Result<TcpListener> {
    let addr = tokio::net::lookup_host((host, port))
        .await?
        .next()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                format!("{host} resolves to no address"),
            )
        })?;
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, None)?;
    if addr.is_ipv6() {
        socket.set_only_v6(false)?;
    }
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(BACKLOG)?;
    TcpListener::from_std(socket.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_unspecified_ipv6_address_accepts_ipv4_and_ipv6_clients() {
        let listener = bind("::", 0).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        for client in ["127.0.0.1", "::1"] {
            tokio::net::TcpStream::connect((client, port))
                .await
                .unwrap();
            listener.accept().await.unwrap();
        }
    }
}
