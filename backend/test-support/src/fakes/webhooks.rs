/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use async_trait::async_trait;
use gradient_core::webhooks::WebhookClient;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct RecordedWebhookCall {
    pub url: String,
    pub signature: String,
    pub event: String,
    pub body: String,
}

/// In-memory `WebhookClient` for tests. Records every delivery and returns
/// `default_status` (200 by default).
#[derive(Debug)]
pub struct RecordingWebhookClient {
    pub calls: Mutex<Vec<RecordedWebhookCall>>,
    pub default_status: u16,
}

impl Default for RecordingWebhookClient {
    fn default() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            default_status: 200,
        }
    }
}

impl RecordingWebhookClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_status(status: u16) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            default_status: status,
        }
    }

    pub fn calls(&self) -> Vec<RecordedWebhookCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl WebhookClient for RecordingWebhookClient {
    async fn deliver(
        &self,
        url: &str,
        signature: &str,
        event: &str,
        body: String,
    ) -> Result<u16> {
        self.calls.lock().unwrap().push(RecordedWebhookCall {
            url: url.to_string(),
            signature: signature.to_string(),
            event: event.to_string(),
            body,
        });
        Ok(self.default_status)
    }
}
