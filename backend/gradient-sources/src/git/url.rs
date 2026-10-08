/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::SourceError;

pub(super) fn parse_git_protocol_url(url: &str) -> Result<(&str, u16, &str), SourceError> {
    let rest = url.strip_prefix("git://").ok_or(SourceError::InvalidUrl)?;
    let (host_port, repo_path) = rest.split_once('/').ok_or(SourceError::InvalidUrl)?;
    let (host, port) = if let Some((h, p)) = host_port.rsplit_once(':') {
        (h, p.parse::<u16>().unwrap_or(9418))
    } else {
        (host_port, 9418u16)
    };
    Ok((host, port, repo_path))
}

/// libgit2 has no `git+https` or `git+http` transport. It is misrouting such a URL to SSH, and the
/// connect is failing with "invalid argument port".
pub(super) fn git_transport_url(url: &str) -> &str {
    url.strip_prefix("git+").unwrap_or(url)
}

/// The local transport cannot fetch shallow. A local repository is cheap to read whole.
pub(super) fn set_shallow_unless_local(fetch: &mut git2::FetchOptions<'_>, url: &str) {
    if !url.starts_with("file://") && !url.starts_with('/') {
        fetch.depth(1);
    }
}
