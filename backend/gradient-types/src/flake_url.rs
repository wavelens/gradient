/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;
use std::str::FromStr;

use crate::input::InputError;

/// `builtins.getFlake` is accepting `ssh://`, `http://` and `https://` URLs only with a
/// `git+` prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryUrl {
    url: String,
}

impl RepositoryUrl {
    fn normalize(url: &str) -> String {
        if url.starts_with("ssh://")
            || url.starts_with("http://")
            || url.starts_with("https://")
            || url.starts_with("git://")
        {
            format!("git+{}", url)
        } else {
            url.to_string()
        }
    }

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

        match s.split_once("://") {
            Some((scheme, rest)) => check_authority(scheme, rest)?,
            None if !s.contains('@') && !s.contains(':') => {
                return Err(InputError::InvalidRepositoryUrl);
            }
            None => {}
        }

        Ok(Self {
            url: Self::normalize(s),
        })
    }
}

fn check_authority(scheme: &str, rest: &str) -> Result<(), InputError> {
    let authority = rest
        .split_once('/')
        .map_or(rest, |(authority, _)| authority);
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let (host, port) = split_host_port(host_port).ok_or(InputError::InvalidRepositoryUrl)?;

    if host.is_empty() {
        return Err(InputError::InvalidRepositoryUrl);
    }

    match port {
        Some(port) if port.parse::<u16>().is_err() => Err(InputError::RepositoryUrlPortInvalid {
            scheme: scheme.to_owned(),
            port: port.to_owned(),
        }),
        _ => Ok(()),
    }
}

fn split_host_port(host_port: &str) -> Option<(&str, Option<&str>)> {
    match host_port.strip_prefix('[') {
        Some(bracketed) => {
            let (host, after) = bracketed.split_once(']')?;
            Some((host, after.strip_prefix(':')))
        }
        None => Some(match host_port.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host_port, None),
        }),
    }
}

impl fmt::Display for RepositoryUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.url)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NixFlakeUrl {
    url: String,
    rev: String,
}

impl NixFlakeUrl {
    pub fn new(repository_url: &str, commit_hash: &str) -> Result<Self, InputError> {
        repository_url
            .parse::<RepositoryUrl>()?
            .with_rev(commit_hash)
    }

    pub fn url(&self) -> &str {
        &self.url
    }

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

    #[test]
    fn repo_url_remote_transport_gets_git_prefix() {
        let cases = [
            (
                "https://github.com/foo/bar.git",
                "git+https://github.com/foo/bar.git",
            ),
            (
                "http://example.com/repo.git",
                "git+http://example.com/repo.git",
            ),
            (
                "ssh://git@github.com/foo/bar.git",
                "git+ssh://git@github.com/foo/bar.git",
            ),
            (
                "git://server.example.com/repo.git",
                "git+git://server.example.com/repo.git",
            ),
            ("git@github.com:foo/bar.git", "git@github.com:foo/bar.git"),
        ];
        for (input, expected) in cases {
            let url: RepositoryUrl = input.parse().unwrap();
            assert_eq!(url.to_string(), expected, "{input}");
        }
    }

    #[test]
    fn repo_url_local_or_unparsable_is_rejected() {
        for input in ["", "file:///local/repo", "notaurl"] {
            assert!(input.parse::<RepositoryUrl>().is_err(), "{input}");
        }
        assert_eq!(
            "file:/local/repo".parse::<RepositoryUrl>().unwrap_err(),
            InputError::LocalFileUrlNotAllowed,
        );
    }

    #[test]
    fn repo_url_scheme_with_scp_path_names_the_bad_port() {
        assert_eq!(
            "ssh://git@git.example.de:user/repo.git"
                .parse::<RepositoryUrl>()
                .unwrap_err(),
            InputError::RepositoryUrlPortInvalid {
                scheme: "ssh".into(),
                port: "user".into(),
            },
        );
        assert_eq!(
            "https://host:8o8o/repo"
                .parse::<RepositoryUrl>()
                .unwrap_err(),
            InputError::RepositoryUrlPortInvalid {
                scheme: "https".into(),
                port: "8o8o".into(),
            },
        );
    }

    #[test]
    fn repo_url_scheme_with_port_or_ipv6_host_is_accepted() {
        for input in [
            "ssh://git@git.example.de:2222/user/repo.git",
            "git+ssh://git@[::1]:22/srv/repo",
            "ssh://[::1]/srv/repo",
            "https://git.example.de:8443/user/repo",
            "git@git.example.de:user/repo.git",
        ] {
            assert!(input.parse::<RepositoryUrl>().is_ok(), "{input}");
        }
    }

    #[test]
    fn repo_url_scheme_without_host_is_rejected() {
        for input in ["https:///repo", "ssh://git@/repo", "ssh://[::1/repo"] {
            assert_eq!(
                input.parse::<RepositoryUrl>().unwrap_err(),
                InputError::InvalidRepositoryUrl,
                "{input}"
            );
        }
    }

    #[test]
    fn nix_url_normalizes_repository_and_appends_rev() {
        let cases = [
            (
                "git@github.com:Wavelens/Gradient.git",
                "git@github.com:Wavelens/Gradient.git",
            ),
            (
                "https://github.com/Wavelens/Gradient.git",
                "git+https://github.com/Wavelens/Gradient.git",
            ),
        ];
        for (input, expected) in cases {
            let u = NixFlakeUrl::new(input, REV).unwrap();
            assert_eq!(u.url(), expected, "{input}");
            assert_eq!(u.to_string(), format!("{expected}?rev={REV}"), "{input}");
        }
    }

    #[test]
    fn nix_url_invalid_input_is_rejected() {
        assert!(NixFlakeUrl::new("https://github.com/foo/bar.git", "abc123").is_err());
        assert!(NixFlakeUrl::new("file:///local/repo", REV).is_err());
    }

    #[test]
    fn with_rev_roundtrip() {
        let r: RepositoryUrl = "https://github.com/foo/bar.git".parse().unwrap();
        let u = r.with_rev(REV).unwrap();
        assert_eq!(
            u.to_string(),
            format!("git+https://github.com/foo/bar.git?rev={REV}")
        );
    }
}
