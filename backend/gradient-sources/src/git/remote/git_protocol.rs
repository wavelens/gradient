/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::AdvertisedRef;
use crate::SourceError;
use crate::git::pktline::{MAX_GIT_REF_ADVERTISEMENT_READ_BYTE_COUNT, read_advertised_refs};
use crate::git::url::parse_git_protocol_url;
use std::io::{self, Read};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const LIST_DEADLINE: Duration = Duration::from_secs(120);

struct TcpStreamWithDeadline {
    stream: TcpStream,
    deadline: Instant,
}

impl Read for TcpStreamWithDeadline {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "git server did not finish within {} seconds",
                        LIST_DEADLINE.as_secs()
                    ),
                )
            })?;
        self.stream
            .set_read_timeout(Some(remaining.min(READ_TIMEOUT)))?;
        self.stream.read(buf)
    }
}

fn connect_tcp_with_timeout(host: &str, port: u16) -> std::io::Result<TcpStream> {
    let mut last_error = std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Host name resolved to no addresses",
    );
    for addr in (host, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(e) => last_error = e,
        }
    }
    Err(last_error)
}

/// libgit2 is negotiating protocol v2 with git-daemon, and its `ls-refs` exchange can fail
/// silently. This plain v0 request without `version=2` is getting an immediate ref advertisement
/// with HEAD.
pub(super) fn list_refs_git_protocol(url: &str) -> Result<Vec<AdvertisedRef>, SourceError> {
    use std::io::Write;

    let (host, port, repo_path) = parse_git_protocol_url(url)?;

    let failed = |e: io::Error| SourceError::GitCommandFailed {
        stderr: e.to_string(),
    };
    let deadline = Instant::now() + LIST_DEADLINE;
    let mut stream = connect_tcp_with_timeout(host, port).map_err(failed)?;
    stream
        .set_write_timeout(Some(READ_TIMEOUT))
        .map_err(failed)?;

    let body = format!("git-upload-pack /{}\0host={}\0", repo_path, host);
    let pkt = format!("{:04x}{}", body.len() + 4, body);
    stream.write_all(pkt.as_bytes()).map_err(failed)?;

    read_advertised_refs(
        &mut TcpStreamWithDeadline { stream, deadline },
        MAX_GIT_REF_ADVERTISEMENT_READ_BYTE_COUNT,
    )
}
