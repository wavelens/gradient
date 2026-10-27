/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io;

const DEFAULT_USER: &str = "git";
const DEFAULT_PORT: u16 = 22;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SshTarget {
    pub user: String,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl SshTarget {
    pub fn parse(url: &str) -> io::Result<Self> {
        let target = match url.split_once("://") {
            Some((_, rest)) => Self::from_url(rest),
            None => Self::from_scp(url),
        };

        target
            .filter(|t| !t.host.is_empty() && !t.path.is_empty())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "malformed SSH URL"))
    }

    pub fn command(&self, program: &str) -> String {
        let path = self.path.strip_prefix('/').filter(|p| p.starts_with('~'));
        let quoted = path
            .unwrap_or(&self.path)
            .replace('\'', r"'\''")
            .replace('!', r"'\!'");
        format!("{program} '{quoted}'")
    }

    fn from_url(rest: &str) -> Option<Self> {
        let (authority, path) = rest.split_at(rest.find('/')?);
        let (user, host_port) = split_user(authority);
        let (host, port) = match host_port.rsplit_once(':') {
            Some((host, port)) if !port.contains(']') => (host, port.parse().ok()?),
            _ => (host_port, DEFAULT_PORT),
        };
        Some(Self::new(user, host, port, path))
    }

    fn from_scp(url: &str) -> Option<Self> {
        let (user, rest) = split_user(url);
        let (host, path) = match rest.strip_prefix('[') {
            Some(bracketed) => bracketed.split_once("]:")?,
            None => rest.split_once(':')?,
        };
        Some(Self::new(user, host, DEFAULT_PORT, path))
    }

    fn new(user: Option<&str>, host: &str, port: u16, path: &str) -> Self {
        Self {
            user: user.unwrap_or(DEFAULT_USER).to_owned(),
            host: host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_owned(),
            port,
            path: path.to_owned(),
        }
    }
}

fn split_user(authority: &str) -> (Option<&str>, &str) {
    match authority.rsplit_once('@') {
        Some((user, host)) => (Some(user), host),
        None => (None, authority),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(user: &str, host: &str, port: u16, path: &str) -> SshTarget {
        SshTarget {
            user: user.to_owned(),
            host: host.to_owned(),
            port,
            path: path.to_owned(),
        }
    }

    #[test]
    fn ssh_url_with_user_and_port() {
        assert_eq!(
            SshTarget::parse("ssh://deploy@git.example.com:2222/org/repo.git").unwrap(),
            target("deploy", "git.example.com", 2222, "/org/repo.git")
        );
    }

    #[test]
    fn ssh_url_without_user_logs_in_as_git() {
        assert_eq!(
            SshTarget::parse("ssh://[::1]/srv/repo").unwrap(),
            target("git", "::1", 22, "/srv/repo")
        );
    }

    #[test]
    fn scp_like_path_stays_relative() {
        assert_eq!(
            SshTarget::parse("git@github.com:org/repo.git").unwrap(),
            target("git", "github.com", 22, "org/repo.git")
        );
    }

    #[test]
    fn url_without_path_is_rejected() {
        assert!(SshTarget::parse("ssh://github.com").is_err());
        assert!(SshTarget::parse("github.com:").is_err());
    }

    #[test]
    fn command_quotes_the_path_for_the_remote_shell() {
        let repo = target("git", "h", 22, "/it's here!");
        assert_eq!(
            repo.command("git-upload-pack"),
            r"git-upload-pack '/it'\''s here'\!''"
        );
    }

    #[test]
    fn command_drops_the_slash_before_a_home_directory() {
        let repo = target("git", "h", 22, "/~alice/repo");
        assert_eq!(
            repo.command("git-upload-pack"),
            "git-upload-pack '~alice/repo'"
        );
    }
}
