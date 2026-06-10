/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{accept_cert, find_ref_in_list};
use crate::SourceError;
use git2::{Direction, RemoteCallbacks};

/// List the remote HEAD ref via libgit2 with no credentials (for https://).
pub(super) fn ls_remote_head_no_creds(
    url: &str,
    branch: Option<&str>,
) -> Result<Vec<u8>, SourceError> {
    let mut remote =
        git2::Remote::create_detached(url).map_err(|e| SourceError::GitCommand(e.to_string()))?;

    let mut callbacks = RemoteCallbacks::new();
    callbacks.certificate_check(|cert, _valid| Ok(accept_cert(cert)));

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
