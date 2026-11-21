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
    use std::net::{IpAddr, Ipv4Addr};
    use tokio::net::TcpStream;

    #[tokio::test]
    async fn unspecified_ipv6_address_accepts_ipv4_clients() {
        let listener = bind("::", 0).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let (_, peer) = listener.accept().await.unwrap();

        assert_eq!(peer.ip().to_canonical(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    #[tokio::test]
    async fn the_unspecified_ipv6_address_accepts_ipv6_clients_when_the_host_has_the_ipv6_loopback_address()
     {
        let listener = bind("::", 0).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        match TcpStream::connect(("::1", port)).await {
            Err(error) if error.kind() == io::ErrorKind::AddrNotAvailable => {
                eprintln!(
                    "IPv6 part doesn't run because this host has no ::1 loopback address ({error})"
                );
                return;
            }
            connected => connected.unwrap(),
        };
        listener.accept().await.unwrap();
    }
}
