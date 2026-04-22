/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! GitHub App manifest flow: build the manifest JSON, construct the manifest
//! POST URL, and exchange the temporary code GitHub returned for the new
//! App's credentials. See:
//! https://docs.github.com/en/apps/sharing-github-apps/registering-a-github-app-from-a-manifest

use serde::{Deserialize, Serialize};

/// Credentials returned by GitHub after a successful manifest exchange.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestResult {
    pub id: i64,
    pub slug: String,
    pub html_url: String,
    pub pem: String,
    pub webhook_secret: String,
    pub client_id: String,
    pub client_secret: String,
}

use serde_json::{Value, json};

/// Default GitHub App name used in the manifest. The operator can edit the
/// name on the GitHub-side review screen before confirming.
pub const APP_NAME: &str = "Gradient";

/// Builds the manifest JSON payload that gets POSTed to GitHub at
/// `https://{host}/settings/apps/new`. `serve_url` is the externally
/// reachable Gradient URL; trailing slashes are stripped.
pub fn build_manifest(serve_url: &str) -> Value {
    let base = serve_url.trim_end_matches('/');
    json!({
        "name": APP_NAME,
        "url": base,
        "hook_attributes": {
            "url": format!("{base}/api/v1/hooks/github"),
            "active": true,
        },
        "redirect_url": format!("{base}/api/v1/admin/github-app/callback"),
        "setup_url": format!("{base}/admin/github-app"),
        "setup_on_update": false,
        "public": false,
        "default_permissions": {
            "metadata": "read",
            "contents": "read",
            "statuses": "write",
            "checks": "write",
            "pull_requests": "read",
        },
        "default_events": [
            "push",
            "pull_request",
            "installation",
            "installation_repositories",
        ],
    })
}

/// URL the browser POSTs the manifest form to. Same path on github.com and
/// any GitHub Enterprise host.
pub fn manifest_post_url(host: &str, state: &str) -> String {
    format!("https://{host}/settings/apps/new?state={state}")
}

/// API base URL for the manifest-conversion endpoint. github.com uses the
/// `api.github.com` subdomain; Enterprise hosts use `/api/v3` on the host.
pub fn api_base_url(host: &str) -> String {
    if host == "github.com" {
        "https://api.github.com".to_string()
    } else {
        format!("https://{host}/api/v3")
    }
}

use anyhow::{Context, Result, bail};
use tracing::debug;

/// Exchanges the temporary code GitHub returned for the new App's credentials.
///
/// `host` is the GitHub host (`github.com` for github.com, `ghe.example.com`
/// for an Enterprise instance); the API base URL is derived via
/// [`api_base_url`].
pub async fn exchange_code(host: &str, code: &str) -> Result<ManifestResult> {
    let base = api_base_url(host);
    exchange_code_with_base(&base, code).await
}

/// Lower-level exchange entry point that accepts an explicit API base URL.
/// Used by tests against `wiremock`.
pub async fn exchange_code_with_base(api_base_url: &str, code: &str) -> Result<ManifestResult> {
    let url = format!("{api_base_url}/app-manifests/{code}/conversions");
    debug!(%url, "exchanging github app manifest code");

    let client = reqwest::Client::builder()
        .user_agent("gradient-ci/1.0")
        .build()
        .context("failed to build reqwest client")?;

    let resp = client
        .post(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .context("github manifest exchange request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("GitHub manifest exchange returned {status}: {body}");
    }

    resp.json::<ManifestResult>()
        .await
        .context("failed to parse manifest exchange response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_manifest_strips_trailing_slash() {
        let with = build_manifest("https://example.com/");
        let without = build_manifest("https://example.com");
        assert_eq!(with, without);
    }

    #[test]
    fn build_manifest_uses_serve_url_in_all_url_fields() {
        let m = build_manifest("https://gradient.example.com");
        assert_eq!(m["url"], "https://gradient.example.com");
        assert_eq!(
            m["hook_attributes"]["url"],
            "https://gradient.example.com/api/v1/hooks/github"
        );
        assert_eq!(
            m["redirect_url"],
            "https://gradient.example.com/api/v1/admin/github-app/callback"
        );
        assert_eq!(m["setup_url"], "https://gradient.example.com/admin/github-app");
    }

    #[test]
    fn build_manifest_has_default_permissions_and_events() {
        let m = build_manifest("https://x.test");
        assert_eq!(m["name"], "Gradient");
        assert_eq!(m["public"], false);
        assert_eq!(m["hook_attributes"]["active"], true);
        assert_eq!(m["setup_on_update"], false);
        assert_eq!(m["default_permissions"]["metadata"], "read");
        assert_eq!(m["default_permissions"]["contents"], "read");
        assert_eq!(m["default_permissions"]["statuses"], "write");
        assert_eq!(m["default_permissions"]["checks"], "write");
        assert_eq!(m["default_permissions"]["pull_requests"], "read");
        assert_eq!(
            m["default_events"],
            json!(["push", "pull_request", "installation", "installation_repositories"])
        );
    }

    #[test]
    fn manifest_post_url_github_com() {
        assert_eq!(
            manifest_post_url("github.com", "abc123"),
            "https://github.com/settings/apps/new?state=abc123"
        );
    }

    #[test]
    fn manifest_post_url_enterprise_host() {
        assert_eq!(
            manifest_post_url("ghe.example.com", "tok"),
            "https://ghe.example.com/settings/apps/new?state=tok"
        );
    }

    #[test]
    fn api_base_url_github_com() {
        assert_eq!(api_base_url("github.com"), "https://api.github.com");
    }

    #[test]
    fn api_base_url_enterprise() {
        assert_eq!(
            api_base_url("ghe.example.com"),
            "https://ghe.example.com/api/v3"
        );
    }

    #[tokio::test]
    async fn exchange_code_happy_path() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/app-manifests/abc/conversions"))
            .respond_with(wiremock::ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 42,
                "slug": "my-app",
                "html_url": "https://github.com/apps/my-app",
                "pem": "----- PEM -----",
                "webhook_secret": "whsec",
                "client_id": "cid",
                "client_secret": "csec",
            })))
            .mount(&server)
            .await;

        let result = exchange_code_with_base(&server.uri(), "abc")
            .await
            .expect("happy path");
        assert_eq!(result.id, 42);
        assert_eq!(result.slug, "my-app");
        assert_eq!(result.pem, "----- PEM -----");
        assert_eq!(result.webhook_secret, "whsec");
    }

    #[tokio::test]
    async fn exchange_code_non_2xx_errors() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/app-manifests/bad/conversions"))
            .respond_with(wiremock::ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let err = exchange_code_with_base(&server.uri(), "bad")
            .await
            .expect_err("404 should error");
        let msg = format!("{err}");
        assert!(msg.contains("404"));
    }
}
