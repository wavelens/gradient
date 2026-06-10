/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! GitLab provider. GitLab webhooks authenticate with a shared secret token
//! (`X-Gitlab-Token`) compared in constant time, not an HMAC signature.

use std::sync::Arc;

use anyhow::anyhow;
use subtle::ConstantTimeEq;

use gradient_types::ForgeType;
use crate::provider::ForgeProvider;
use crate::reporter::{CiReporter, GitlabReporter};
use crate::webhook::{
    ParsedPullRequestEvent, ParsedPushEvent, ParsedReleaseEvent, WebhookEventKind,
};

#[derive(Debug)]
pub struct GitlabProvider;

impl ForgeProvider for GitlabProvider {
    fn forge_type(&self) -> ForgeType {
        ForgeType::GitLab
    }

    fn build_reporter(
        &self,
        http: reqwest::Client,
        endpoint_url: Option<&str>,
        token: Option<&str>,
    ) -> anyhow::Result<Arc<dyn CiReporter>> {
        let base_url = endpoint_url
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("GitLab integration missing endpoint_url"))?;
        let token = token.ok_or_else(|| anyhow!("GitLab integration missing token"))?;

        Ok(Arc::new(GitlabReporter::new(http, base_url, token)?))
    }

    fn signature_header(&self) -> &'static str {
        "X-Gitlab-Token"
    }

    fn verify_signature(&self, secret: &str, signature: &str, _body: &[u8]) -> bool {
        signature.as_bytes().ct_eq(secret.as_bytes()).into()
    }

    fn event_headers(&self) -> &'static [&'static str] {
        &["X-Gitlab-Event"]
    }

    fn classify_event(&self, event: &str) -> WebhookEventKind {
        match event {
            "Push Hook" | "Tag Push Hook" => WebhookEventKind::Push,
            "Merge Request Hook" => WebhookEventKind::PullRequest,
            "Release Hook" => WebhookEventKind::Release,
            "Note Hook" => WebhookEventKind::Comment,
            other => WebhookEventKind::Unknown(other.to_string()),
        }
    }

    fn parse_push_event(&self, body: &[u8]) -> Option<ParsedPushEvent> {
        ParsedPushEvent::from_gitlab(body)
    }

    fn parse_pull_request_event(&self, body: &[u8]) -> Option<ParsedPullRequestEvent> {
        ParsedPullRequestEvent::from_gitlab(body)
    }

    fn parse_release_event(&self, body: &[u8]) -> Option<ParsedReleaseEvent> {
        ParsedReleaseEvent::from_gitlab(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_token_exactly() {
        assert!(GitlabProvider.verify_signature("s3cret", "s3cret", b""));
    }

    #[test]
    fn rejects_mismatched_token() {
        assert!(!GitlabProvider.verify_signature("s3cret", "wrong", b""));
    }

    #[test]
    fn rejects_missing_token() {
        assert!(!GitlabProvider.verify_signature("s3cret", "", b""));
    }
}
