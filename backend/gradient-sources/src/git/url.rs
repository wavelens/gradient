/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::SourceError;
use gradient_types::RepositoryUrl;

pub(super) const GIT_PROTOCOL_DEFAULT_PORT: u16 = 9418;

pub(super) fn parse_git_protocol_url(url: &str) -> Result<RepositoryUrl, SourceError> {
    url.parse::<RepositoryUrl>()
        .ok()
        .filter(|url| {
            url.git_connection_url().starts_with("git://") && !url.repository_path().is_empty()
        })
        .ok_or(SourceError::InvalidUrl)
}

/// The local transport cannot fetch shallow. A local repository is cheap to read whole.
pub(super) fn set_shallow_unless_local(fetch: &mut git2::FetchOptions<'_>, url: &str) {
    if !url.starts_with("file://") && !url.starts_with('/') {
        fetch.depth(1);
    }
}
