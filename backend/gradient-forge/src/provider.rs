/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The per-forge behaviour seam. Every forge-specific decision - which reporter
//! to build, how to verify a webhook signature, which header carries the event,
//! how to parse each payload - lives behind this trait, so the rest of the
//! codebase dispatches through [`ForgeRegistry`](crate::ForgeRegistry)
//! instead of matching on [`ForgeType`].

use std::sync::Arc;

use gradient_types::ForgeType;
use crate::reporter::CiReporter;
use crate::webhook::{
    ParsedPullRequestEvent, ParsedReleaseEvent, PushOutcome, WebhookEventKind,
};

pub trait ForgeProvider: Send + Sync + std::fmt::Debug {
    /// The forge variant this provider serves.
    fn forge_type(&self) -> ForgeType;

    /// Build a token/PAT-based status reporter from a configured integration's
    /// `endpoint_url` and access token. App-style auth (GitHub) is resolved by
    /// the caller; see [`ForgeProvider::supports_app_auth`].
    fn build_reporter(
        &self,
        http: reqwest::Client,
        endpoint_url: Option<&str>,
        token: Option<&str>,
    ) -> anyhow::Result<Arc<dyn CiReporter>>;

    /// Whether this forge supports GitHub-App-style installation auth. The CI
    /// layer probes this before falling back to a token reporter.
    fn supports_app_auth(&self) -> bool {
        false
    }

    /// Whether this forge is served by the per-integration `/hooks/{forge}/…`
    /// endpoint. GitHub goes through its dedicated App webhook instead.
    fn accepts_per_integration_webhook(&self) -> bool {
        true
    }

    /// Header(s) carrying the webhook signature/token, tried in order (first
    /// present wins), passed to [`verify_signature`](Self::verify_signature).
    fn signature_headers(&self) -> &'static [&'static str];

    /// Verify a webhook signature/token (HMAC for Gitea/GitHub, constant-time
    /// token equality for GitLab) against the integration secret.
    fn verify_signature(&self, secret: &str, signature: &str, body: &[u8]) -> bool;

    /// Header(s) carrying the event name, tried in order (first present wins).
    fn event_headers(&self) -> &'static [&'static str];

    /// Map a raw forge event string onto the shared [`WebhookEventKind`].
    fn classify_event(&self, event: &str) -> WebhookEventKind;

    fn parse_push_event(&self, body: &[u8]) -> Option<PushOutcome>;
    fn parse_pull_request_event(&self, body: &[u8]) -> Option<ParsedPullRequestEvent>;
    fn parse_release_event(&self, body: &[u8]) -> Option<ParsedReleaseEvent>;
}
