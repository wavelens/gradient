/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::fetch_source_allowlist_and_request_validation::AllowedFetchRequest;
use gradient_types::https_git_url_host::https_host_from_repository_url;
use gradient_types::input::check_repository_url_is_ssh;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRequest {
    pub fetch_key: String,
    pub canonical_input_request_json: String,
    pub needs_secrets: bool,
    pub https_git_host: Option<String>,
    pub content_hash_store_path: Option<String>,
}

impl InputRequest {
    pub fn parse(request: &str) -> Result<Self, String> {
        let request = AllowedFetchRequest::parse(request).map_err(|e| e.to_string())?;
        let canonical_input_request_json = request.get_canonical_fetch_request_json();
        let content_hash_store_path = request.pin();
        if let Some(pin) = &content_hash_store_path
            && let Some(claimed) = request.store_path()
            && claimed != pin
        {
            return Err(format!(
                "Store path {claimed} does not match the store path {pin} from the content hash of the request."
            ));
        }
        let https_git_host =
            serde_json::from_str::<serde_json::Value>(&canonical_input_request_json)
                .is_ok_and(|json| json["args"]["type"] == "git")
                .then(|| {
                    request
                        .url_to_fetch()
                        .and_then(https_host_from_repository_url)
                })
                .flatten();
        Ok(Self {
            fetch_key: content_hash_store_path
                .clone()
                .unwrap_or_else(|| canonical_input_request_json.clone()),
            needs_secrets: request
                .url_to_fetch()
                .is_some_and(check_repository_url_is_ssh),
            https_git_host,
            content_hash_store_path,
            canonical_input_request_json,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_without_a_store_path_share_a_key_regardless_of_attribute_order() {
        let first = InputRequest::parse(
            r#"{"builtin":"fetchTree","args":{"type":"git","url":"git+ssh://git@example.com/r","rev":"abc"}}"#,
        )
        .unwrap();
        let second = InputRequest::parse(
            r#"{"args":{"rev":"abc","url":"git+ssh://git@example.com/r","type":"git"},"builtin":"fetchTree"}"#,
        )
        .unwrap();

        assert_eq!(first.fetch_key, second.fetch_key);
        assert!(first.needs_secrets);
    }

    const NAR_HASH: &str = "sha256-OXhFkURii0pxIkZ2DeaOUN+tybxn+ccJvfDKIsA2UXk=";

    fn github_fetch_tree_request_json_with_optional_store_path(store_path: Option<&str>) -> String {
        let mut request = serde_json::json!({
            "builtin": "fetchTree",
            "args": {"type": "github", "owner": "o", "repo": "r", "narHash": NAR_HASH},
        });
        if let Some(store_path) = store_path {
            request["storePath"] = store_path.into();
        }
        request.to_string()
    }

    fn nix_store_path_from_fetch_request_content_hash(request: &str) -> String {
        AllowedFetchRequest::parse(request).unwrap().pin().unwrap()
    }

    #[test]
    fn fetch_key_equals_content_pin_when_worker_store_path_matches() {
        let request = github_fetch_tree_request_json_with_optional_store_path(None);
        let pin = nix_store_path_from_fetch_request_content_hash(&request);

        let parsed = InputRequest::parse(&request).unwrap();
        assert_eq!(parsed.fetch_key, pin);
        assert_eq!(
            parsed.content_hash_store_path.as_deref(),
            Some(pin.as_str())
        );
        assert_eq!(
            InputRequest::parse(&github_fetch_tree_request_json_with_optional_store_path(
                Some(&pin)
            ))
            .unwrap()
            .fetch_key,
            pin
        );
    }

    #[test]
    fn refuses_a_request_whose_store_path_disagrees_with_the_nar_hash() {
        let request = github_fetch_tree_request_json_with_optional_store_path(Some(
            "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-source",
        ));

        assert!(InputRequest::parse(&request).is_err());
    }

    #[test]
    fn fetch_request_without_content_hash_does_not_use_worker_store_path_as_fetch_key() {
        let request = InputRequest::parse(
            r#"{"builtin":"fetchTree","args":{"type":"tarball","url":"https://example.com/x.tar.gz"},"storePath":"/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-source"}"#,
        )
        .unwrap();

        assert_eq!(request.fetch_key, request.canonical_input_request_json);
        assert_eq!(request.content_hash_store_path, None);
        assert!(!request.needs_secrets);
        assert!(InputRequest::parse(r#"{"builtin":"fetchTree"}"#).is_err());
    }

    #[test]
    fn parsing_git_input_request_sets_https_host_only_for_https_urls() {
        let https = InputRequest::parse(
            r#"{"builtin":"fetchTree","args":{"type":"git","url":"https://Git.Example.org:8443/r.git"}}"#,
        )
        .unwrap();
        let ssh = InputRequest::parse(
            r#"{"builtin":"fetchTree","args":{"type":"git","url":"git+ssh://git@example.com/r"}}"#,
        )
        .unwrap();

        assert!(!https.needs_secrets);
        assert_eq!(
            https.https_git_host.as_deref(),
            Some("git.example.org:8443")
        );
        assert_eq!(ssh.https_git_host, None);
    }

    #[test]
    fn requests_for_host_files_are_refused() {
        for request in [
            r#"{"builtin":"fetchTree","args":{"type":"path","path":"/run/credentials/x","narHash":"sha256-AAAA"}}"#,
            r#"{"builtin":"fetchTarball","args":{"url":"file:///etc/passwd","name":"source"}}"#,
            r#"{"builtin":"fetchTree","args":{"type":"tarball","url":"file:///etc/passwd"}}"#,
            r#"{"builtin":"fetchTree","args":{"type":"git","url":"/srv/repo"}}"#,
        ] {
            assert!(InputRequest::parse(request).is_err(), "{request}");
        }
    }

    #[test]
    fn the_canonical_request_is_the_parsed_one() {
        let request = InputRequest::parse(
            r#"{"args":{"url":"https://example.com/x.tar.gz","type":"tarball"},"builtin":"fetchTree"}"#,
        )
        .unwrap();

        assert_eq!(
            request.canonical_input_request_json,
            r#"{"builtin":"fetchTree","args":{"type":"tarball","url":"https://example.com/x.tar.gz"}}"#
        );
    }
}
