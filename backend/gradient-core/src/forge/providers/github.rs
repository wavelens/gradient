/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! GitHub provider. Inbound webhooks arrive on the dedicated GitHub App
//! endpoint, so this provider opts out of the per-integration webhook route;
//! App-installation auth is resolved by the CI layer via [`supports_app_auth`].
//!
//! [`supports_app_auth`]: ForgeProvider::supports_app_auth

use std::sync::Arc;

use anyhow::anyhow;

use gradient_types::ForgeType;
use crate::forge::github_app::verify_github_signature;
use crate::forge::provider::ForgeProvider;
use crate::forge::reporter::{CiReporter, GithubReporter};
use crate::forge::webhook::{
    ParsedPullRequestEvent, ParsedPushEvent, ParsedReleaseEvent, WebhookEventKind,
};

#[derive(Debug)]
pub struct GithubProvider;

impl ForgeProvider for GithubProvider {
    fn forge_type(&self) -> ForgeType {
        ForgeType::GitHub
    }

    fn build_reporter(
        &self,
        http: reqwest::Client,
        endpoint_url: Option<&str>,
        token: Option<&str>,
    ) -> anyhow::Result<Arc<dyn CiReporter>> {
        let token = token.ok_or_else(|| anyhow!("GitHub integration missing token"))?;

        Ok(Arc::new(GithubReporter::new(
            http,
            endpoint_url.unwrap_or(""),
            token,
        )?))
    }

    fn supports_app_auth(&self) -> bool {
        true
    }

    fn accepts_per_integration_webhook(&self) -> bool {
        false
    }

    fn signature_header(&self) -> &'static str {
        "X-Hub-Signature-256"
    }

    fn verify_signature(&self, secret: &str, signature: &str, body: &[u8]) -> bool {
        verify_github_signature(secret, signature, body)
    }

    fn event_headers(&self) -> &'static [&'static str] {
        &["X-GitHub-Event"]
    }

    fn classify_event(&self, _event: &str) -> WebhookEventKind {
        WebhookEventKind::Unknown("github".into())
    }

    fn parse_push_event(&self, body: &[u8]) -> Option<ParsedPushEvent> {
        ParsedPushEvent::from_github(body)
    }

    fn parse_pull_request_event(&self, body: &[u8]) -> Option<ParsedPullRequestEvent> {
        ParsedPullRequestEvent::from_github(body)
    }

    fn parse_release_event(&self, body: &[u8]) -> Option<ParsedReleaseEvent> {
        ParsedReleaseEvent::from_github(body)
    }
}
