/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;
use std::str::FromStr;

#[derive(Debug, thiserror::Error)]
pub enum InvalidHttpBaseUrlError {
    #[error("Invalid URL: {0}")]
    Parse(#[from] url::ParseError),
    #[error("The URL scheme can only be http or https. Got '{0}'.")]
    Scheme(String),
    #[error("Include a host in the URL.")]
    MissingHost,
    #[error("The base URL can't include a username or password.")]
    Userinfo,
    #[error("The base URL can't include a query.")]
    Query,
    #[error("The base URL can't contain a fragment.")]
    Fragment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedHttpBaseUrl(String);

impl ValidatedHttpBaseUrl {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn append_api_path_to_base_url(&self, path: &str) -> String {
        if path.is_empty() || path.starts_with('/') {
            format!("{}{path}", self.0)
        } else {
            format!("{}/{path}", self.0)
        }
    }
}

impl FromStr for ValidatedHttpBaseUrl {
    type Err = InvalidHttpBaseUrlError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let parsed = url::Url::parse(raw.trim())?;
        match parsed.scheme() {
            "http" | "https" => {}
            s => return Err(InvalidHttpBaseUrlError::Scheme(s.to_owned())),
        }
        if parsed.host_str().is_none_or(str::is_empty) {
            return Err(InvalidHttpBaseUrlError::MissingHost);
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(InvalidHttpBaseUrlError::Userinfo);
        }
        if parsed.query().is_some() {
            return Err(InvalidHttpBaseUrlError::Query);
        }
        if parsed.fragment().is_some() {
            return Err(InvalidHttpBaseUrlError::Fragment);
        }
        Ok(Self(parsed.as_str().trim_end_matches('/').to_owned()))
    }
}

impl fmt::Display for ValidatedHttpBaseUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamCacheUrlForDisplay {
    pub url: String,
    pub had_removed_url_parts: bool,
}

/// A stored URL can still contain a user and a password. A query token can remain in that URL. All responses, exports and log text pass the URL through this function before display. The function can never display text that cannot parse as a URL.
pub fn upstream_cache_url_for_display_without_username_password_query_and_fragment(
    stored: &str,
) -> Option<UpstreamCacheUrlForDisplay> {
    let mut parsed = url::Url::parse(stored.trim()).ok()?;
    let had_removed_url_parts = !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some();
    if !had_removed_url_parts {
        return Some(UpstreamCacheUrlForDisplay {
            url: stored.to_owned(),
            had_removed_url_parts,
        });
    }
    parsed.set_username("").ok()?;
    parsed.set_password(None).ok()?;
    parsed.set_query(None);
    parsed.set_fragment(None);
    Some(UpstreamCacheUrlForDisplay {
        url: parsed.as_str().to_owned(),
        had_removed_url_parts,
    })
}

pub fn presigned_url_without_signature_credential_user_info_and_fragment_for_error_and_log_messages_or_placeholder(
    url: &str,
) -> String {
    upstream_cache_url_for_display_without_username_password_query_and_fragment(url)
        .map_or_else(|| "unparseable URL".to_owned(), |shown| shown.url)
}

pub fn remove_user_name_and_password_from_urls_in_error_text(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|word| {
            let url = word[find_url_scheme_start_index(word)..]
                .trim_matches(|c: char| c.is_whitespace() || "'\"<>()[]{},;:".contains(c));
            match upstream_cache_url_for_display_without_username_password_query_and_fragment(url) {
                Some(shown) if shown.had_removed_url_parts => word.replacen(url, &shown.url, 1),
                _ => word.to_owned(),
            }
        })
        .collect()
}

fn find_url_scheme_start_index(word: &str) -> usize {
    word.find("://").map_or(0, |separator| {
        word[..separator]
            .char_indices()
            .rev()
            .find(|&(_, c)| !(c.is_ascii_alphanumeric() || "+-.".contains(c)))
            .map_or(0, |(index, c)| index + c.len_utf8())
    })
}

pub fn urls_differ_in_scheme_host_or_port(first: &str, second: &str) -> bool {
    let origin = |raw: &str| {
        let url = url::Url::parse(raw).ok()?;
        let host = url.host_str()?.to_owned();
        Some((url.scheme().to_owned(), host, url.port_or_known_default()))
    };
    match (origin(first), origin(second)) {
        (Some(first), Some(second)) => first != second,
        _ => true,
    }
}

pub fn url_origin_without_credentials(url: &str) -> Option<String> {
    let url = url::Url::parse(url.trim()).ok()?;
    let scheme = url.scheme();
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_that_differ_only_in_their_path_are_not_reported_as_different() {
        assert!(!urls_differ_in_scheme_host_or_port(
            "https://h:1",
            "https://h:1/other"
        ));
        assert!(!urls_differ_in_scheme_host_or_port(
            "https://h",
            "https://h:443"
        ));
        assert!(urls_differ_in_scheme_host_or_port("https://h", "http://h"));
        assert!(urls_differ_in_scheme_host_or_port("https://h", "https://g"));
        assert!(urls_differ_in_scheme_host_or_port(
            "https://h:1",
            "https://h:2"
        ));
    }

    #[test]
    fn urls_where_at_least_one_has_no_host_are_reported_as_different() {
        assert!(urls_differ_in_scheme_host_or_port(
            "mailto:first@example.com",
            "mailto:second@example.com"
        ));
        assert!(urls_differ_in_scheme_host_or_port(
            "file://server/share",
            "file:///share"
        ));
    }

    #[test]
    fn test_removes_user_and_password_from_url_in_error_text() {
        let text =
            "failed to fetch 'https://user:secret@host/path': unexpected http status code: 401";
        assert_eq!(
            remove_user_name_and_password_from_urls_in_error_text(text),
            "failed to fetch 'https://host/path': unexpected http status code: 401"
        );
    }

    #[test]
    fn test_remove_presigned_query_from_wrapped_urls_keeps_surrounding_punctuation() {
        let url = "https://host/bucket/key?X-Amz-Credential=AKIAEXAMPLE%2F20260101&X-Amz-Signature=abc123";
        for (text, expected) in [
            (
                format!("error sending request for url ({url})"),
                "error sending request for url (https://host/bucket/key)",
            ),
            (
                format!("PUT {url}, retrying"),
                "PUT https://host/bucket/key, retrying",
            ),
            (
                format!("{url}: timed out"),
                "https://host/bucket/key: timed out",
            ),
            (
                format!("failed \"{url}\""),
                "failed \"https://host/bucket/key\"",
            ),
            (format!("('{url}')"), "('https://host/bucket/key')"),
            (format!("url={url};"), "url=https://host/bucket/key;"),
            (
                format!("{{\"url\":\"{url}\"}}"),
                "{\"url\":\"https://host/bucket/key\"}",
            ),
            (format!("«{url} »"), "«https://host/bucket/key »"),
        ] {
            let scrubbed = remove_user_name_and_password_from_urls_in_error_text(&text);
            assert_eq!(scrubbed, expected, "{text}");
        }
    }

    #[test]
    fn a_base_url_is_normalised_and_joins_paths() {
        let base: ValidatedHttpBaseUrl = "https://Cache.Example.com/prefix/".parse().unwrap();

        assert_eq!(base.as_str(), "https://cache.example.com/prefix");
        assert_eq!(
            base.append_api_path_to_base_url("nix-cache-info"),
            "https://cache.example.com/prefix/nix-cache-info"
        );
        assert_eq!(
            base.append_api_path_to_base_url("/api/v1/repos?state=open"),
            "https://cache.example.com/prefix/api/v1/repos?state=open"
        );
    }

    #[test]
    fn a_base_url_refuses_parts_that_could_swallow_the_appended_path() {
        for raw in [
            "https://cache.example.com/?x=",
            "https://cache.example.com/#",
            "https://user:pw@cache.example.com/",
            "https://user@cache.example.com/",
            "ftp://cache.example.com/",
            "file:///etc",
            "not a url",
        ] {
            assert!(raw.parse::<ValidatedHttpBaseUrl>().is_err(), "{raw}");
        }
    }

    #[test]
    fn display_form_drops_user_password_query_and_fragment_and_returns_none_for_non_urls() {
        for (stored, shown) in [
            ("https://u:p@h.example/prefix", "https://h.example/prefix"),
            ("https://u@h.example/", "https://h.example/"),
            ("https://h.example/?token=x", "https://h.example/"),
            ("https://h.example/#frag", "https://h.example/"),
        ] {
            let result =
                upstream_cache_url_for_display_without_username_password_query_and_fragment(stored)
                    .unwrap();
            assert_eq!(result.url, shown, "{stored}");
            assert!(result.had_removed_url_parts, "{stored}");
        }
        let clean = upstream_cache_url_for_display_without_username_password_query_and_fragment(
            "https://h.example/prefix",
        )
        .unwrap();
        assert_eq!(clean.url, "https://h.example/prefix");
        assert!(!clean.had_removed_url_parts);
        assert_eq!(
            upstream_cache_url_for_display_without_username_password_query_and_fragment(
                "not a url u:p@h?token=x"
            ),
            None
        );
    }
}
