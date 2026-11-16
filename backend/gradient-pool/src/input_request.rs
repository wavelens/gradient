/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::https_git_url_host::https_host_from_repository_url;
use gradient_types::input::check_repository_url_is_ssh;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRequest {
    pub fetch_key: String,
    pub needs_secrets: bool,
    pub https_git_host: Option<String>,
}

impl InputRequest {
    pub fn parse(request: &str) -> Result<Self, String> {
        let json: Value =
            serde_json::from_str(request).map_err(|e| format!("malformed input request: {e}"))?;
        let builtin = json["builtin"]
            .as_str()
            .ok_or("an input request without a builtin")?;
        let args = &json["args"];
        if !args.is_object() {
            return Err("an input request without arguments".to_owned());
        }

        let fetch_key = json["storePath"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                sorted(&serde_json::json!({ "builtin": builtin, "args": args })).to_string()
            });
        let url = args["url"].as_str();
        let https_git_host = (args["type"] == "git")
            .then(|| url.and_then(https_host_from_repository_url))
            .flatten();
        Ok(Self {
            fetch_key,
            needs_secrets: url.is_some_and(check_repository_url_is_ssh),
            https_git_host,
        })
    }
}

fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().map(|(k, v)| (k.clone(), sorted(v))).collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
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

    #[test]
    fn a_known_store_path_is_the_key_and_https_needs_no_secrets() {
        let request = InputRequest::parse(
            r#"{"builtin":"fetchTree","args":{"type":"tarball","url":"https://example.com/x.tar.gz"},"storePath":"/nix/store/aaaa-source"}"#,
        )
        .unwrap();

        assert_eq!(request.fetch_key, "/nix/store/aaaa-source");
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
}
