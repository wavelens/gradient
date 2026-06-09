/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::sources::SourceError;
use crate::sources::git::pktline::read_ref_from_pktlines;
use crate::sources::git::url::parse_git_protocol_url;

/// List the remote HEAD ref using the raw git wire protocol (v0) over TCP.
///
/// libgit2's `connect_auth` + `list()` can return an empty ref list for
/// `git://` URLs because it negotiates git protocol v2 with git-daemon, and
/// the subsequent `ls-refs` exchange may fail silently on some daemon versions.
/// This implementation sends a plain protocol-v0 pkt-line request (no
/// `version=2` extra parameter) so the daemon responds with an immediate v0
/// ref advertisement containing HEAD.
pub(super) fn ls_remote_head_git_protocol(
    url: &str,
    branch: Option<&str>,
) -> Result<Vec<u8>, SourceError> {
    use std::io::Write;
    use std::net::TcpStream;
    use std::time::Duration;

    let (host, port, repo_path) = parse_git_protocol_url(url)?;

    let mut stream =
        TcpStream::connect((host, port)).map_err(|e| SourceError::GitCommandFailed {
            stderr: e.to_string(),
        })?;

    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| SourceError::GitCommandFailed {
            stderr: e.to_string(),
        })?;

    // Protocol-v0 request: "git-upload-pack /path\0host=host\0"
    // Deliberately omitting "version=2" so the daemon responds in v0 format.
    let body = format!("git-upload-pack /{}\0host={}\0", repo_path, host);
    let pkt = format!("{:04x}{}", body.len() + 4, body);
    stream
        .write_all(pkt.as_bytes())
        .map_err(|e| SourceError::GitCommandFailed {
            stderr: e.to_string(),
        })?;

    let target = branch.map(|b| format!("refs/heads/{}", b));
    read_ref_from_pktlines(&mut stream, target.as_deref())
}
