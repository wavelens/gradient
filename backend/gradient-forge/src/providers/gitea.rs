/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Gitea + Forgejo provider (identical webhook and status-API surface).

use std::sync::Arc;

use anyhow::anyhow;

use gradient_types::ForgeType;
use crate::github_app::verify_gitea_signature;
use crate::provider::ForgeProvider;
use crate::reporter::{CiReporter, GiteaReporter};
use crate::webhook::{
    ParsedPullRequestEvent, ParsedPushEvent, ParsedReleaseEvent, PushOutcome, WebhookEventKind,
};

#[derive(Debug)]
pub struct GiteaProvider {
    forge: ForgeType,
}

impl GiteaProvider {
    pub fn new(forge: ForgeType) -> Self {
        Self { forge }
    }
}

impl ForgeProvider for GiteaProvider {
    fn forge_type(&self) -> ForgeType {
        self.forge
    }

    fn build_reporter(
        &self,
        http: reqwest::Client,
        endpoint_url: Option<&str>,
        token: Option<&str>,
    ) -> anyhow::Result<Arc<dyn CiReporter>> {
        let base_url = endpoint_url
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("Gitea/Forgejo integration missing endpoint_url"))?;
        let token = token.ok_or_else(|| anyhow!("Gitea/Forgejo integration missing token"))?;

        Ok(Arc::new(GiteaReporter::new(http, base_url, token)?))
    }

    fn signature_header(&self) -> &'static str {
        "X-Gitea-Signature"
    }

    fn verify_signature(&self, secret: &str, signature: &str, body: &[u8]) -> bool {
        verify_gitea_signature(secret, signature, body)
    }

    fn event_headers(&self) -> &'static [&'static str] {
        &["X-Gitea-Event", "X-Gogs-Event"]
    }

    fn classify_event(&self, event: &str) -> WebhookEventKind {
        match event {
            "push" => WebhookEventKind::Push,
            "pull_request" => WebhookEventKind::PullRequest,
            "release" => WebhookEventKind::Release,
            "issue_comment" | "pull_request_comment" => WebhookEventKind::Comment,
            "pull_request_review" => WebhookEventKind::Review,
            other => WebhookEventKind::Unknown(other.to_string()),
        }
    }

    fn parse_push_event(&self, body: &[u8]) -> Option<PushOutcome> {
        ParsedPushEvent::from_gitea(body)
    }

    fn parse_pull_request_event(&self, body: &[u8]) -> Option<ParsedPullRequestEvent> {
        ParsedPullRequestEvent::from_gitea(body)
    }

    fn parse_release_event(&self, body: &[u8]) -> Option<ParsedReleaseEvent> {
        ParsedReleaseEvent::from_gitea(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_signature() {
        let p = GiteaProvider::new(ForgeType::Gitea);
        assert!(!p.verify_signature("s3cret", "", b"body"));
    }
}
