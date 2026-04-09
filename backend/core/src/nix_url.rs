/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;
use std::str::FromStr;

use crate::input::InputError;

/// A validated, normalized repository URL.
///
/// Rejects local `file://` references and bare/invalid inputs. Normalizes
/// `ssh://`, `http://`, and `https://` URLs by prepending `git+` so they are
/// accepted by `builtins.getFlake`.
///
/// Use [`RepositoryUrl::with_rev`] to pin it to a specific commit and produce
/// a [`NixFlakeUrl`].
///
/// # Example
///
/// ```
/// use core::nix_url::RepositoryUrl;
///
/// let r: RepositoryUrl = "https://github.com/foo/bar.git".parse().unwrap();
/// assert_eq!(r.to_string(), "git+https://github.com/foo/bar.git");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryUrl {
    url: String,
}

impl RepositoryUrl {
    fn normalize(url: &str) -> String {
        if url.starts_with("ssh://")
            || url.starts_with("http://")
            || url.starts_with("https://")
        {
            format!("git+{}", url)
        } else {
            url.to_string()
        }
    }

    /// Combine with a 40-character commit hash to produce a [`NixFlakeUrl`].
    pub fn with_rev(self, commit_hash: &str) -> Result<NixFlakeUrl, InputError> {
        if commit_hash.len() != 40 {
            return Err(InputError::InvalidCommitHashLength);
        }
        Ok(NixFlakeUrl {
            url: self.url,
            rev: commit_hash.to_string(),
        })
    }
}

impl FromStr for RepositoryUrl {
    type Err = InputError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(InputError::InvalidRepositoryUrl);
        }

        if s.contains("file://") || s.starts_with("file") {
            return Err(InputError::LocalFileUrlNotAllowed);
        }

        // Require at least one host separator: either `://`, `@`, or `:` (SCP style).
        let looks_like_url = s.contains("://") || s.contains('@') || s.contains(':');
        if !looks_like_url {
            return Err(InputError::InvalidRepositoryUrl);
        }

        Ok(Self {
            url: Self::normalize(s),
        })
    }
}

impl fmt::Display for RepositoryUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.url)
    }
}

/// A validated, normalized Nix flake URL pinned to a specific commit.
///
/// Produced by [`RepositoryUrl::with_rev`] or [`NixFlakeUrl::new`].
/// `Display` yields the `<url>?rev=<hash>` form consumed by
/// `builtins.getFlake` and related Nix tooling.
///
/// # Example
///
/// ```
/// use core::nix_url::NixFlakeUrl;
///
/// let u = NixFlakeUrl::new("https://github.com/foo/bar.git",
///                          "11c2f8505c234697ccabbc96e5b8a76daf0f31d3").unwrap();
/// assert_eq!(
///     u.to_string(),
///     "git+https://github.com/foo/bar.git?rev=11c2f8505c234697ccabbc96e5b8a76daf0f31d3"
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NixFlakeUrl {
    /// Normalized repository URL (`git+` prefix applied where needed).
    url: String,
    /// 40-character hex commit hash.
    rev: String,
}

impl NixFlakeUrl {
    /// Build a `NixFlakeUrl` from a raw repository URL and a 40-character commit hash.
    pub fn new(repository_url: &str, commit_hash: &str) -> Result<Self, InputError> {
        repository_url.parse::<RepositoryUrl>()?.with_rev(commit_hash)
    }

    /// The normalized repository URL (without the `?rev=…` suffix).
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The pinned commit hash.
    pub fn rev(&self) -> &str {
        &self.rev
    }
}

impl fmt::Display for NixFlakeUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}?rev={}", self.url, self.rev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REV: &str = "11c2f8505c234697ccabbc96e5b8a76daf0f31d3";

    // ── RepositoryUrl ────────────────────────────────────────────────────────

    #[test]
    fn repo_url_https_normalized() {
        let r: RepositoryUrl = "https://github.com/foo/bar.git".parse().unwrap();
        assert_eq!(r.to_string(), "git+https://github.com/foo/bar.git");
    }

    #[test]
    fn repo_url_http_normalized() {
        let r: RepositoryUrl = "http://example.com/repo.git".parse().unwrap();
        assert_eq!(r.to_string(), "git+http://example.com/repo.git");
    }

    #[test]
    fn repo_url_ssh_protocol_normalized() {
        let r: RepositoryUrl = "ssh://git@github.com/foo/bar.git".parse().unwrap();
        assert_eq!(r.to_string(), "git+ssh://git@github.com/foo/bar.git");
    }

    #[test]
    fn repo_url_scp_passthrough() {
        let r: RepositoryUrl = "git@github.com:foo/bar.git".parse().unwrap();
        assert_eq!(r.to_string(), "git@github.com:foo/bar.git");
    }

    #[test]
    fn repo_url_git_protocol_passthrough() {
        let r: RepositoryUrl = "git://server.example.com/repo.git".parse().unwrap();
        assert_eq!(r.to_string(), "git://server.example.com/repo.git");
    }

    #[test]
    fn repo_url_empty_rejected() {
        assert!("".parse::<RepositoryUrl>().is_err());
    }

    #[test]
    fn repo_url_file_rejected() {
        assert!("file:///local/repo".parse::<RepositoryUrl>().is_err());
    }

    #[test]
    fn repo_url_plain_string_rejected() {
        assert!("notaurl".parse::<RepositoryUrl>().is_err());
    }

    // ── NixFlakeUrl ──────────────────────────────────────────────────────────

    #[test]
    fn nix_url_ssh_scp_style() {
        let u = NixFlakeUrl::new("git@github.com:Wavelens/Gradient.git", REV).unwrap();
        assert_eq!(u.url(), "git@github.com:Wavelens/Gradient.git");
        assert_eq!(u.to_string(), format!("git@github.com:Wavelens/Gradient.git?rev={REV}"));
    }

    #[test]
    fn nix_url_https_gets_git_plus_prefix() {
        let u = NixFlakeUrl::new("https://github.com/Wavelens/Gradient.git", REV).unwrap();
        assert_eq!(u.url(), "git+https://github.com/Wavelens/Gradient.git");
        assert_eq!(u.to_string(), format!("git+https://github.com/Wavelens/Gradient.git?rev={REV}"));
    }

    #[test]
    fn nix_url_short_hash_rejected() {
        assert!(NixFlakeUrl::new("https://github.com/foo/bar.git", "abc123").is_err());
    }

    #[test]
    fn nix_url_file_rejected() {
        assert!(NixFlakeUrl::new("file:///local/repo", REV).is_err());
    }

    #[test]
    fn nix_url_rev_accessor() {
        let u = NixFlakeUrl::new("git@github.com:foo/bar.git", REV).unwrap();
        assert_eq!(u.rev(), REV);
    }

    #[test]
    fn with_rev_roundtrip() {
        let r: RepositoryUrl = "https://github.com/foo/bar.git".parse().unwrap();
        let u = r.with_rev(REV).unwrap();
        assert_eq!(u.to_string(), format!("git+https://github.com/foo/bar.git?rev={REV}"));
    }
}
