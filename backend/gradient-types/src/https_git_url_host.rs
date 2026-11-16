/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use url::Url;

pub fn https_host_from_repository_url(url: &str) -> Option<String> {
    let url = url.strip_prefix("git+").unwrap_or(url);
    let host = parsed_https_url_host_and_port(url)?;
    let written = read_https_url_host_and_port_as_written(url)?;
    host_text_matches_normalized_host_ignoring_ascii_case_and_default_https_port(written, &host)
        .then_some(host)
}

pub fn normalize_bare_https_git_host(host: &str) -> Option<String> {
    let normalized = https_host_from_repository_url(&format!("https://{host}"))?;
    host_text_matches_normalized_host_ignoring_ascii_case_and_default_https_port(host, &normalized)
        .then_some(normalized)
}

fn parsed_https_url_host_and_port(url: &str) -> Option<String> {
    let url = Url::parse(url).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

fn read_https_url_host_and_port_as_written(url: &str) -> Option<&str> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority = rest
        .split_once('/')
        .map_or(rest, |(authority, _)| authority);
    Some(
        authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host),
    )
}

fn host_text_matches_normalized_host_ignoring_ascii_case_and_default_https_port(
    written: &str,
    host: &str,
) -> bool {
    written.eq_ignore_ascii_case(host)
        || written
            .strip_suffix(":443")
            .is_some_and(|written| written.eq_ignore_ascii_case(host))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_https_url_yields_its_lowercase_host() {
        assert_eq!(
            https_host_from_repository_url("https://GitHub.com/a/b").as_deref(),
            Some("github.com")
        );
    }

    #[test]
    fn a_non_default_port_is_kept_and_the_default_port_is_dropped() {
        assert_eq!(
            https_host_from_repository_url("https://git.example:8443/x").as_deref(),
            Some("git.example:8443")
        );
        assert_eq!(
            https_host_from_repository_url("https://git.example:443/x").as_deref(),
            Some("git.example")
        );
    }

    #[test]
    fn user_info_and_the_flake_prefix_do_not_change_the_host() {
        assert_eq!(
            https_host_from_repository_url("git+https://token@Git.Example/x?dir=sub").as_deref(),
            Some("git.example")
        );
    }

    #[test]
    fn every_other_scheme_has_no_https_host() {
        for url in [
            "http://github.com/a/b",
            "ssh://git@github.com/a/b",
            "git+ssh://git@github.com/a/b",
            "git@github.com:a/b.git",
            "git://github.com/a/b",
            "github:a/b",
            "not a url",
        ] {
            assert_eq!(https_host_from_repository_url(url), None, "{url}");
        }
    }

    #[test]
    fn urls_with_backslash_hash_or_question_before_at_evil_com_yield_no_host() {
        for url in [
            "https://github.com\\@evil.com/x",
            "https://github.com#@evil.com/x",
            "https://github.com?@evil.com/x",
            "https://github.com?@evil.com",
        ] {
            assert_eq!(https_host_from_repository_url(url), None, "{url}");
        }
    }

    #[test]
    fn urls_with_normalised_hosts_have_no_host() {
        for url in [
            "https://git\thub.com/x",
            "https://git\nhub.com/x",
            "https://github%2Ecom/x",
            "https://github.com /x",
            " https://github.com/x",
        ] {
            assert_eq!(https_host_from_repository_url(url), None, "{url:?}");
        }
    }

    #[test]
    fn test_ipv6_host_in_brackets_keeps_non_default_port_and_drops_default_port_443() {
        assert_eq!(
            https_host_from_repository_url("https://[::1]:8443/x").as_deref(),
            Some("[::1]:8443")
        );
        assert_eq!(
            https_host_from_repository_url("https://[::1]:443/x").as_deref(),
            Some("[::1]")
        );
    }

    #[test]
    fn test_bare_host_is_normalized_and_keeps_non_default_port() {
        for (host, expected) in [
            ("GitHub.com", "github.com"),
            ("git.example:8443", "git.example:8443"),
            ("git.example:443", "git.example"),
            ("[::1]:8443", "[::1]:8443"),
        ] {
            assert_eq!(
                normalize_bare_https_git_host(host).as_deref(),
                Some(expected),
                "{host}"
            );
        }
    }

    #[test]
    fn rejects_bare_host_inputs_that_are_not_host_with_optional_port() {
        for host in [
            "user@github.com",
            "github.com/x",
            "https://github.com",
            "",
            " github.com",
            "github.com ",
            "git hub.com",
            "github.com:",
            "github.com#x",
            "github.com?x",
            "github%2Ecom",
        ] {
            assert_eq!(normalize_bare_https_git_host(host), None, "{host:?}");
        }
    }
}
