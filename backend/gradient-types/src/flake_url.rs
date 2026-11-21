/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;
use std::str::FromStr;

use gradient_util::http_base_url::upstream_cache_url_for_display_without_username_password_query_and_fragment;

use crate::input::InputError;

const ALLOWED_TASK_REPOSITORY_URL_SCHEMES: [&str; 4] = ["https", "http", "ssh", "git"];
const GIT_REPOSITORY_FLAKE_URL_PREFIX: &str = "git+";

/// `builtins.getFlake` is accepting `ssh://`, `http://` and `https://` URLs only with a
/// `git+` prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryUrl {
    url: String,
    git_connection_url: String,
    git_server_host_name: String,
    git_server_port: Option<u16>,
    repository_path: String,
}

impl RepositoryUrl {
    pub fn git_connection_url(&self) -> &str {
        &self.git_connection_url
    }

    pub fn git_server_host_name(&self) -> &str {
        &self.git_server_host_name
    }

    pub fn git_server_port(&self) -> Option<u16> {
        self.git_server_port
    }

    pub fn repository_path(&self) -> &str {
        &self.repository_path
    }

    pub fn git_connection_url_for_log_without_username_password_and_query(&self) -> String {
        upstream_cache_url_for_display_without_username_password_query_and_fragment(
            &self.git_connection_url,
        )
        .map_or_else(
            || format!("{}:{}", self.git_server_host_name, self.repository_path),
            |shown| shown.url,
        )
    }

    #[cfg(any(test, feature = "test-functions"))]
    pub fn local_test_repository(directory: &std::path::Path) -> Self {
        let url = format!("file://{}", directory.display());
        Self {
            url: url.clone(),
            git_connection_url: url,
            git_server_host_name: String::new(),
            git_server_port: None,
            repository_path: directory.display().to_string(),
        }
    }

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
        if s.is_empty()
            || s.chars()
                .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '#' | '\\'))
        {
            return Err(InputError::InvalidRepositoryUrl);
        }

        if s.contains("file://") || s.starts_with("file") {
            return Err(InputError::LocalFileUrlNotAllowed);
        }

        let clone_url = RepositoryLocation::check(s)?.clone_url;
        let (transport_url, address) = match clone_url.split_once("://") {
            Some((scheme, rest)) => {
                let transport_scheme = scheme
                    .strip_prefix(GIT_REPOSITORY_FLAKE_URL_PREFIX)
                    .unwrap_or(scheme);
                if !ALLOWED_TASK_REPOSITORY_URL_SCHEMES.contains(&transport_scheme) {
                    return Err(InputError::InvalidRepositoryUrl);
                }
                (
                    clone_url
                        .strip_prefix(GIT_REPOSITORY_FLAKE_URL_PREFIX)
                        .unwrap_or(clone_url),
                    parse_repository_url_host_port_path(scheme, rest)?,
                )
            }
            None => (
                clone_url,
                parse_ssh_host_port_path(clone_url).ok_or(InputError::InvalidRepositoryUrl)?,
            ),
        };

        Ok(Self {
            url: Self::normalize(s),
            git_connection_url: transport_url.to_owned(),
            git_server_host_name: address.host.to_owned(),
            git_server_port: address.port,
            repository_path: address.path.to_owned(),
        })
    }
}

struct RepositoryHostPortPath<'a> {
    host: &'a str,
    port: Option<u16>,
    path: &'a str,
}

fn parse_ssh_host_port_path(url: &str) -> Option<RepositoryHostPortPath<'_>> {
    let (user, host_and_path) = url.split_once('@')?;
    let (host, path) = match host_and_path.strip_prefix('[') {
        Some(bracketed) => {
            let (host, after) = bracketed.split_once(']')?;
            (host, after.strip_prefix(':')?)
        }
        None => host_and_path.split_once(':')?,
    };
    let is_valid = !user.is_empty()
        && !user.contains([':', '/'])
        && !user.starts_with('-')
        && !host.is_empty()
        && !host.starts_with('-')
        && !host.contains(['/', '@'])
        && !path.is_empty();
    is_valid.then_some(RepositoryHostPortPath {
        host,
        port: None,
        path,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepositoryLocation<'a> {
    pub clone_url: &'a str,
    pub flake_dir: Option<&'a str>,
}

impl<'a> RepositoryLocation<'a> {
    pub fn of(url: &'a str) -> Self {
        let (clone_url, query) = split_query(url);
        Self {
            clone_url,
            flake_dir: query_pairs(query)
                .find(|(key, _)| *key == "dir")
                .map(|(_, value)| value),
        }
    }

    pub fn check(url: &'a str) -> Result<Self, InputError> {
        let (_, query) = split_query(url);
        if let Some((key, _)) = query_pairs(query).find(|(key, _)| *key != "dir") {
            return Err(InputError::UnsupportedRepositoryQuery(key.to_owned()));
        }
        let location = Self::of(url);
        if let Some(dir) = location.flake_dir {
            check_flake_dir(dir)?;
        }
        Ok(location)
    }
}

fn split_query(url: &str) -> (&str, &str) {
    url.split_once('?').unwrap_or((url, ""))
}

fn query_pairs(query: &str) -> impl Iterator<Item = (&str, &str)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
}

pub fn parse_owner_repo(repository_url: &str) -> Option<(String, String)> {
    let clone_url = RepositoryLocation::of(repository_url).clone_url;
    let url = clone_url.strip_prefix("git+").unwrap_or(clone_url);

    let path = if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("git://"))
        .or_else(|| url.strip_prefix("ssh://"))
    {
        rest.split_once('/')?.1
    } else {
        let colon_pos = url.find(':')?;
        &url[colon_pos + 1..]
    };

    let path = path.trim_end_matches('/').trim_end_matches(".git");
    if !path.split('/').all(is_plain_name_segment) {
        return None;
    }
    let (owner, repo) = path.split_once('/')?;
    Some((owner.to_string(), repo.to_string()))
}

pub fn check_flake_dir(dir: &str) -> Result<(), InputError> {
    if dir.split('/').all(is_plain_name_segment) {
        Ok(())
    } else {
        Err(InputError::InvalidFlakeDir(dir.to_owned()))
    }
}

pub fn is_plain_name_segment(segment: &str) -> bool {
    !matches!(segment, "" | "." | "..")
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

pub(crate) fn check_authority(scheme: &str, rest: &str) -> Result<(), InputError> {
    parse_repository_url_host_port_path(scheme, rest).map(|_| ())
}

fn parse_repository_url_host_port_path<'a>(
    scheme: &str,
    rest: &'a str,
) -> Result<RepositoryHostPortPath<'a>, InputError> {
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (user, host_port) = authority.rsplit_once('@').unwrap_or(("", authority));
    let (host, port) = split_host_port(host_port).ok_or(InputError::InvalidRepositoryUrl)?;

    if host.is_empty() || user.starts_with('-') || host.starts_with('-') {
        return Err(InputError::InvalidRepositoryUrl);
    }

    let port = port
        .map(|port| {
            port.parse::<u16>()
                .map_err(|_| InputError::RepositoryUrlPortInvalid {
                    scheme: scheme.to_owned(),
                    port: port.to_owned(),
                })
        })
        .transpose()?;
    Ok(RepositoryHostPortPath { host, port, path })
}

pub(crate) fn split_host_port(host_port: &str) -> Option<(&str, Option<&str>)> {
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
    fn refused_repository_url_with_disallowed_scheme_or_character_is_reported_as_invalid() {
        for input in [
            "path:/etc",
            "git+file:/x",
            "github:o/r",
            "ftp://h/o/r",
            "/srv/repo",
            "host:o/r",
            "https://h/o/r\r\nx",
            "ssh://h/o r",
            "git@h:o/r\tx",
            "https://h/o/r\0",
            "https://evil.example#@gitea.example.com/o/r",
            "https://evil.example\\@gitea.example.com/o/r",
            "git@h:o/r#x",
            "ssh://-oProxyCommand=x/r",
            "ssh://-u@h/r",
            "git@-o:r",
            "-u@h:r",
        ] {
            assert_eq!(
                input.parse::<RepositoryUrl>().unwrap_err(),
                InputError::InvalidRepositoryUrl,
                "{input:?}"
            );
        }
    }

    #[test]
    fn git_prefix_urls_and_scp_style_ipv6_urls_are_accepted() {
        for input in [
            "git+https://h/o/r",
            "git+http://h/o/r",
            "git+ssh://git@h/o/r",
            "git+git://h/o/r",
            "git@[::1]:o/r",
        ] {
            assert!(input.parse::<RepositoryUrl>().is_ok(), "{input}");
        }
    }

    #[test]
    fn parsed_repo_url_reports_transport_url_host_port_and_path() {
        let cases = [
            ("https://h/o/r?dir=sub", "https://h/o/r", "h", None, "o/r"),
            (
                "git+ssh://git@[::1]:2222/srv/repo",
                "ssh://git@[::1]:2222/srv/repo",
                "::1",
                Some(2222),
                "srv/repo",
            ),
            (
                "git://h:9419/foo/bar.git",
                "git://h:9419/foo/bar.git",
                "h",
                Some(9419),
                "foo/bar.git",
            ),
            (
                "git@github.com:o/r.git?dir=a/b",
                "git@github.com:o/r.git",
                "github.com",
                None,
                "o/r.git",
            ),
            ("git@[::1]:o/r", "git@[::1]:o/r", "::1", None, "o/r"),
        ];
        for (input, transport_url, host, port, path) in cases {
            let url: RepositoryUrl = input.parse().unwrap();
            assert_eq!(
                (
                    url.git_connection_url(),
                    url.git_server_host_name(),
                    url.git_server_port(),
                    url.repository_path(),
                ),
                (transport_url, host, port, path),
                "{input}"
            );
        }
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
    fn a_dir_query_names_the_flake_directory() {
        let l = RepositoryLocation::check("https://github.com/acme/infra?dir=nix/hosts").unwrap();
        assert_eq!(
            (l.clone_url, l.flake_dir),
            ("https://github.com/acme/infra", Some("nix/hosts"))
        );
        let plain = RepositoryLocation::check("git@github.com:acme/infra.git").unwrap();
        assert_eq!(
            (plain.clone_url, plain.flake_dir),
            ("git@github.com:acme/infra.git", None)
        );
    }

    #[test]
    fn a_flake_directory_outside_the_checkout_is_refused() {
        for dir in [
            "",
            "/etc",
            "../up",
            "a/../../b",
            "a/./b",
            "a/",
            "%2e%2e",
            "a b",
        ] {
            assert!(check_flake_dir(dir).is_err(), "{dir:?}");
        }
        assert!(check_flake_dir("nix/hosts-1.x_y").is_ok());
    }

    #[test]
    fn only_the_dir_query_is_accepted() {
        assert_eq!(
            RepositoryLocation::check("https://h/o/r?ref=main").unwrap_err(),
            InputError::UnsupportedRepositoryQuery("ref".into())
        );
        assert!("https://h/o/r?dir=../x".parse::<RepositoryUrl>().is_err());
        assert!("https://h/o/r?dir=sub".parse::<RepositoryUrl>().is_ok());
    }

    #[test]
    fn display_text_has_no_user_name_or_password_for_https_and_scp_like_urls() {
        let logged = [
            "https://alice:s3cret@git.example.com/o/r",
            "alice@git.example.com:o/r",
            "https://user:token@git.example.com/o/r",
            "git@git.example.com:o/r",
        ]
        .map(|input| {
            input
                .parse::<RepositoryUrl>()
                .unwrap()
                .git_connection_url_for_log_without_username_password_and_query()
        });

        assert_eq!(
            logged,
            [
                "https://git.example.com/o/r",
                "git.example.com:o/r",
                "https://git.example.com/o/r",
                "git.example.com:o/r",
            ],
            "Repository URL in fetch debug log shows username or password"
        );
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

    #[test]
    fn parse_owner_repo_reads_the_owner_and_repo_from_each_transport() {
        for url in [
            "https://github.com/acme/widgets.git",
            "https://github.com/acme/widgets",
            "http://github.com/acme/widgets.git",
            "git://github.com/acme/widgets.git",
            "git@github.com:acme/widgets.git",
            "git+https://github.com/acme/widgets.git",
            "git+ssh://git@github.com/acme/widgets.git",
        ] {
            assert_eq!(
                parse_owner_repo(url),
                Some(("acme".into(), "widgets".into())),
                "{url}"
            );
        }
        assert_eq!(
            parse_owner_repo("ssh://forgejo@gitea.home.lan:2222/internal/infra-home"),
            Some(("internal".into(), "infra-home".into()))
        );
        assert_eq!(
            parse_owner_repo("git@gitea.example.com:group/sub/repo.git"),
            Some(("group".into(), "sub/repo".into()))
        );
    }

    #[test]
    fn a_flake_directory_does_not_change_the_repository_identity() {
        for url in [
            "https://github.com/acme/infra?dir=nix",
            "git@github.com:acme/infra.git?dir=nix",
        ] {
            assert_eq!(
                parse_owner_repo(url),
                Some(("acme".into(), "infra".into())),
                "{url}"
            );
        }
    }

    #[test]
    fn parse_owner_repo_rejects_urls_without_a_repo_path() {
        for url in [
            "https://github.com",
            "https://github.com/acme",
            "ftp-no-colon-owner-repo",
        ] {
            assert_eq!(parse_owner_repo(url), None, "{url}");
        }
    }

    #[test]
    fn parse_owner_repo_rejects_dot_segments_and_odd_characters() {
        for url in [
            "https://github.com/a/b/../../../orgs/x/y",
            "https://github.com/a/./b",
            "https://github.com/a//b",
            "https://github.com/a/b%2F..",
        ] {
            assert_eq!(parse_owner_repo(url), None, "{url}");
        }
    }
}
