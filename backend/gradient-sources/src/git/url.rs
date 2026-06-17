/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::SourceError;

/// Parses a `git://[host[:port]]/repo/path` URL into its host, port, and repo
/// path components. Defaults port to 9418 (git-daemon).
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

/// Translates a nix flake URL into a transport URL libgit2 understands by
/// stripping the `git+` scheme prefix (`git+https://h/r` → `https://h/r`).
/// libgit2 registers no `git+https`/`git+http` transport: it misroutes such a
/// URL to SSH, whose scheme has no default port, and the connect then fails with
/// "invalid argument port". Bare schemes and SCP-style remotes pass through.
pub(super) fn git_transport_url(url: &str) -> &str {
    url.strip_prefix("git+").unwrap_or(url)
}

/// Parses a nix flake URL of the form `git+<scheme>://host/repo?rev=<hash>` into
/// `(git_url, rev)`.  The `git+` prefix is stripped so the returned URL is
/// suitable for direct use with libgit2.
pub(super) fn parse_nix_git_url(nix_url: &str) -> Result<(String, String), SourceError> {
    let url = git_transport_url(nix_url);
    let (base_url, query) = url.split_once('?').ok_or(SourceError::UrlParsing)?;
    let rev = query
        .split('&')
        .find_map(|p| p.strip_prefix("rev="))
        .ok_or(SourceError::MissingHash)?
        .to_string();

    Ok((base_url.to_string(), rev))
}
