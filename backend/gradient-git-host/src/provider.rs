/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use crate::reporter::CiReporter;
use crate::webhook::{ParsedPullRequestEvent, ParsedReleaseEvent, PushOutcome, WebhookEventKind};
use gradient_types::GitHostType;

pub trait GitHostProvider: Send + Sync + std::fmt::Debug {
    fn git_host_type(&self) -> GitHostType;

    fn build_reporter(
        &self,
        http: reqwest::Client,
        endpoint_url: Option<&str>,
        token: Option<&str>,
    ) -> anyhow::Result<Arc<dyn CiReporter>>;

    fn supports_app_auth(&self) -> bool {
        false
    }

    fn accepts_per_integration_webhook(&self) -> bool {
        true
    }

    fn signature_headers(&self) -> &'static [&'static str];

    fn verify_signature(&self, secret: &str, signature: &str, body: &[u8]) -> bool;

    fn event_headers(&self) -> &'static [&'static str];

    fn classify_event(&self, event: &str) -> WebhookEventKind;

    fn parse_push_event(&self, body: &[u8]) -> Option<PushOutcome>;
    fn parse_pull_request_event(&self, body: &[u8]) -> Option<ParsedPullRequestEvent>;
    fn parse_release_event(&self, body: &[u8]) -> Option<ParsedReleaseEvent>;
}
