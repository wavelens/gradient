/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{BranchHead, branch_head};
use crate::SourceError;
use crate::git::pktline::read_advertised_refs;
use crate::git::url::parse_git_protocol_url;

/// libgit2 is negotiating protocol v2 with git-daemon, and its `ls-refs` exchange can fail
/// silently. This plain v0 request without `version=2` is getting an immediate ref advertisement
/// with HEAD.
pub(super) fn ls_remote_head_git_protocol(
    url: &str,
    branch: Option<&str>,
) -> Result<BranchHead, SourceError> {
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

    let body = format!("git-upload-pack /{}\0host={}\0", repo_path, host);
    let pkt = format!("{:04x}{}", body.len() + 4, body);
    stream
        .write_all(pkt.as_bytes())
        .map_err(|e| SourceError::GitCommandFailed {
            stderr: e.to_string(),
        })?;

    branch_head(&read_advertised_refs(&mut stream)?, branch)
}
