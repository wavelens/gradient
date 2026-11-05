/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::output::{ExitKind, Output};
use connector::exec_jobs::{GuestEvent, GuestSocket};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortForward {
    pub local: u16,
    pub guest: u16,
}

pub(crate) fn parse_forward(raw: &str) -> Result<PortForward, String> {
    let port = |text: &str| text.parse::<u16>().ok().filter(|&port| port != 0);
    let parsed = match raw.split_once(':') {
        Some((local, guest)) => port(local).zip(port(guest)),
        None => port(raw).map(|port| (port, port)),
    };
    parsed
        .map(|(local, guest)| PortForward { local, guest })
        .ok_or_else(|| "Pass PORT or LOCAL:PORT (ports 1-65535).".to_owned())
}

pub(crate) async fn start(
    client: &connector::Client,
    job: &str,
    forwards: &[PortForward],
    out: Output,
) {
    for forward in forwards {
        let listener = TcpListener::bind(("127.0.0.1", forward.local))
            .await
            .unwrap_or_else(|e| {
                out.err(
                    ExitKind::Api,
                    format!("Can't listen on 127.0.0.1:{}: {e}", forward.local),
                )
            });
        out.progress(format!(
            "Forwarding 127.0.0.1:{} to port {} of the job.",
            forward.local, forward.guest
        ));
        tokio::spawn(accept_connections(
            listener,
            client.clone(),
            job.to_owned(),
            forward.guest,
            out,
        ));
    }
}

async fn accept_connections(
    listener: TcpListener,
    client: connector::Client,
    job: String,
    guest: u16,
    out: Output,
) {
    while let Ok((connection, _)) = listener.accept().await {
        let client = client.clone();
        let job = job.clone();
        tokio::spawn(async move {
            match client.exec_jobs().open_port_forward(&job, guest).await {
                Ok(socket) => pipe(connection, socket).await,
                Err(e) => out.progress(format!("Port forward to {guest} failed: {e}")),
            }
        });
    }
}

async fn pipe(mut connection: TcpStream, mut socket: GuestSocket) {
    let (mut from_local, mut to_local) = connection.split();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        tokio::select! {
            read = from_local.read(&mut buffer) => match read {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if socket.send_bytes(buffer[..n].to_vec()).await.is_err() {
                        return;
                    }
                }
            },
            event = socket.next_event() => match event {
                Some(GuestEvent::Output(bytes)) => {
                    if to_local.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
                Some(GuestEvent::Closed { .. }) | None => return,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forward_is_one_port_or_a_local_and_a_guest_port() {
        assert_eq!(
            parse_forward("8888"),
            Ok(PortForward {
                local: 8888,
                guest: 8888
            })
        );
        assert_eq!(
            parse_forward("9000:80"),
            Ok(PortForward {
                local: 9000,
                guest: 80
            })
        );
    }

    #[test]
    fn a_forward_without_valid_ports_is_refused() {
        for raw in ["0", "70000", "80:", ":80", "a:80", "1:2:3", ""] {
            assert!(parse_forward(raw).is_err(), "{raw}");
        }
    }
}
