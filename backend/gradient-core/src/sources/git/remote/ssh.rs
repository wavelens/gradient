/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{accept_cert, find_ref_in_list};
use crate::sources::SourceError;
use git2::{Direction, RemoteCallbacks};

/// List the remote HEAD ref via libgit2 with in-memory SSH credentials.
///
/// Used exclusively for SSH URLs where the private key must be supplied
/// in-memory without writing it to disk.
pub(super) fn ls_remote_head_ssh(
    url: &str,
    private_key: &str,
    public_key: &str,
    branch: Option<&str>,
) -> Result<Vec<u8>, SourceError> {
    let mut remote =
        git2::Remote::create_detached(url).map_err(|e| SourceError::GitCommand(e.to_string()))?;

    let priv_key = private_key.to_string();
    let pub_key = public_key.to_string();
    let mut callbacks = RemoteCallbacks::new();
    callbacks.certificate_check(|cert, _valid| Ok(accept_cert(cert)));
    callbacks.credentials(move |_url, username_from_url, _allowed| {
        git2::Cred::ssh_key_from_memory(
            username_from_url.unwrap_or("git"),
            Some(&pub_key),
            &priv_key,
            None,
        )
    });

    let conn = remote
        .connect_auth(Direction::Fetch, Some(callbacks), None)
        .map_err(|e| SourceError::GitCommandFailed {
            stderr: e.message().to_string(),
        })?;

    let list = conn.list().map_err(|e| SourceError::GitCommandFailed {
        stderr: e.message().to_string(),
    })?;

    find_ref_in_list(list, branch)
}
