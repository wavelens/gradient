/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use anyhow::anyhow;

use crate::github_app::verify_github_signature;
use crate::provider::GitHostProvider;
use crate::reporter::{CiReporter, GithubReporter};
use crate::webhook::{
    ParsedPullRequestEvent, ParsedPushEvent, ParsedReleaseEvent, PushOutcome, WebhookEventKind,
};
use gradient_types::GitHostType;

#[derive(Debug)]
pub struct GithubProvider;

impl GitHostProvider for GithubProvider {
    fn git_host_type(&self) -> GitHostType {
        GitHostType::GitHub
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

    fn signature_headers(&self) -> &'static [&'static str] {
        &["X-Hub-Signature-256"]
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

    fn parse_push_event(&self, body: &[u8]) -> Option<PushOutcome> {
        ParsedPushEvent::from_github(body)
    }

    fn parse_pull_request_event(&self, body: &[u8]) -> Option<ParsedPullRequestEvent> {
        ParsedPullRequestEvent::from_github(body)
    }

    fn parse_release_event(&self, body: &[u8]) -> Option<ParsedReleaseEvent> {
        ParsedReleaseEvent::from_github(body)
    }
}
