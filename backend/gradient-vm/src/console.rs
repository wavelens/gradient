/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tracing::debug;

pub const CONSOLE_LOG: &str = "console.log";
pub const CONSOLE_SOCKET: &str = "console.sock";
pub const SERIAL_SOCKET: &str = "serial.sock";
pub const CONSOLE_TAIL: u64 = 64 * 1024;
const SERIAL_RETRY: Duration = Duration::from_millis(100);
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const READ_CHUNK: usize = 32 * 1024;

pub async fn serve_console(dir: &Path, clients: UnixListener) -> Result<()> {
    let log_path = dir.join(CONSOLE_LOG);
    let log = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .await
        .with_context(|| format!("open {}", log_path.display()))?;
    let mut relay = ConsoleRelay {
        log,
        log_path,
        client: None,
    };
    loop {
        let serial = connect_serial(&dir.join(SERIAL_SOCKET)).await;
        relay.relay(serial, &clients).await?;
    }
}

pub fn console_tail(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(max)))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    Ok(tail)
}

async fn connect_serial(socket: &Path) -> UnixStream {
    loop {
        match UnixStream::connect(socket).await {
            Ok(stream) => return stream,
            Err(_) => tokio::time::sleep(SERIAL_RETRY).await,
        }
    }
}

struct ConsoleClient {
    reader: OwnedReadHalf,
    writer: OwnedWriteHalf,
}

struct ConsoleRelay {
    log: File,
    log_path: std::path::PathBuf,
    client: Option<ConsoleClient>,
}

impl ConsoleRelay {
    async fn relay(&mut self, serial: UnixStream, clients: &UnixListener) -> Result<()> {
        let (mut from_serial, mut to_serial) = serial.into_split();
        let mut serial_buf = vec![0; READ_CHUNK];
        let mut client_buf = vec![0; READ_CHUNK];
        loop {
            tokio::select! {
                read = from_serial.read(&mut serial_buf) => match read {
                    Ok(0) | Err(_) => return Ok(()),
                    Ok(n) => self.on_serial_output(&serial_buf[..n]).await?,
                },
                accepted = clients.accept() => {
                    let (stream, _) = accepted.context("accept a console client")?;
                    self.replace_client(stream).await;
                }
                read = read_client(self.client.as_mut(), &mut client_buf) => match read {
                    Some(n) => {
                        if to_serial.write_all(&client_buf[..n]).await.is_err() {
                            return Ok(());
                        }
                    }
                    None => self.client = None,
                },
            }
        }
    }

    async fn on_serial_output(&mut self, data: &[u8]) -> Result<()> {
        self.log.write_all(data).await?;
        self.log.flush().await?;
        self.send_to_client(data).await;
        Ok(())
    }

    async fn replace_client(&mut self, stream: UnixStream) {
        let (reader, writer) = stream.into_split();
        self.client = Some(ConsoleClient { reader, writer });
        match console_tail(&self.log_path, CONSOLE_TAIL) {
            Ok(tail) => self.send_to_client(&tail).await,
            Err(e) => debug!(error = %e, "no console tail for the new client"),
        }
    }

    async fn send_to_client(&mut self, data: &[u8]) {
        let Some(client) = self.client.as_mut() else {
            return;
        };
        let sent = tokio::time::timeout(CLIENT_WRITE_TIMEOUT, client.writer.write_all(data)).await;
        if !matches!(sent, Ok(Ok(()))) {
            debug!("the console client stopped reading, dropping it");
            self.client = None;
        }
    }
}

async fn read_client(client: Option<&mut ConsoleClient>, buf: &mut [u8]) -> Option<usize> {
    match client {
        Some(client) => match client.reader.read(buf).await {
            Ok(0) | Err(_) => None,
            Ok(n) => Some(n),
        },
        None => std::future::pending().await,
    }
}

pub struct ConsoleLink {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl ConsoleLink {
    pub async fn connect(dir: &Path) -> Result<ConsoleLink> {
        let socket = dir.join(CONSOLE_SOCKET);
        let stream = UnixStream::connect(&socket)
            .await
            .with_context(|| format!("connect to {}", socket.display()))?;
        Ok(ConsoleLink {
            stream,
            buf: vec![0; READ_CHUNK],
        })
    }

    pub async fn read(&mut self) -> Option<Vec<u8>> {
        match self.stream.read(&mut self.buf).await {
            Ok(0) | Err(_) => None,
            Ok(n) => Some(self.buf[..n].to_vec()),
        }
    }

    pub async fn write(&mut self, data: &[u8]) -> Result<()> {
        self.stream.write_all(data).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn read_exactly(link: &mut ConsoleLink, want: &[u8]) {
        let mut got = Vec::new();
        while got.len() < want.len() {
            got.extend(link.read().await.expect("console output"));
        }
        assert_eq!(got, want);
    }

    #[tokio::test]
    async fn a_console_client_gets_the_tail_then_live_output() {
        let dir = tempfile::tempdir().unwrap();
        let serial = UnixListener::bind(dir.path().join(SERIAL_SOCKET)).unwrap();
        let clients = UnixListener::bind(dir.path().join(CONSOLE_SOCKET)).unwrap();
        let log = dir.path().join(CONSOLE_LOG);

        let scenario = async {
            let (mut guest, _) = serial.accept().await.unwrap();
            guest.write_all(b"boot ok\n").await.unwrap();
            while std::fs::read(&log).unwrap_or_default() != b"boot ok\n" {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let mut link = ConsoleLink::connect(dir.path()).await.unwrap();
            read_exactly(&mut link, b"boot ok\n").await;

            guest.write_all(b"login: ").await.unwrap();
            read_exactly(&mut link, b"login: ").await;

            link.write(b"root\n").await.unwrap();
            let mut typed = [0; 5];
            guest.read_exact(&mut typed).await.unwrap();
            assert_eq!(&typed, b"root\n");
        };

        tokio::select! {
            served = serve_console(dir.path(), clients) => panic!("the relay ended: {served:?}"),
            () = scenario => {}
        }
    }

    #[test]
    fn the_tail_starts_inside_a_long_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONSOLE_LOG);
        let mut content = vec![b'x'; 100 * 1024];
        content.extend_from_slice(b"end");
        std::fs::write(&path, content).unwrap();

        let tail = console_tail(&path, 65536).unwrap();

        assert_eq!(tail.len(), 65536);
        assert!(tail.ends_with(b"end"));
    }
}
