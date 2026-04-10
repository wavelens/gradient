/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use async_trait::async_trait;
use gradient_core::storage::EmailSender;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub enum SentEmail {
    Verification {
        to_email: String,
        to_name: String,
        token: String,
        base_url: String,
    },
    PasswordReset {
        to_email: String,
        to_name: String,
        token: String,
        base_url: String,
    },
}

/// In-memory `EmailSender` for tests. Records every send and reports as enabled.
#[derive(Debug, Default)]
pub struct InMemoryEmailSender {
    pub sent: Mutex<Vec<SentEmail>>,
    pub enabled: bool,
}

impl InMemoryEmailSender {
    pub fn new() -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            enabled: true,
        }
    }

    pub fn disabled() -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            enabled: false,
        }
    }

    pub fn sent(&self) -> Vec<SentEmail> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl EmailSender for InMemoryEmailSender {
    fn is_enabled(&self) -> bool {
        self.enabled
    }

    async fn send_verification_email(
        &self,
        to_email: &str,
        to_name: &str,
        verification_token: &str,
        base_url: &str,
    ) -> Result<()> {
        self.sent.lock().unwrap().push(SentEmail::Verification {
            to_email: to_email.to_string(),
            to_name: to_name.to_string(),
            token: verification_token.to_string(),
            base_url: base_url.to_string(),
        });
        Ok(())
    }

    async fn send_password_reset_email(
        &self,
        to_email: &str,
        to_name: &str,
        reset_token: &str,
        base_url: &str,
    ) -> Result<()> {
        self.sent.lock().unwrap().push(SentEmail::PasswordReset {
            to_email: to_email.to_string(),
            to_name: to_name.to_string(),
            token: reset_token.to_string(),
            base_url: base_url.to_string(),
        });
        Ok(())
    }
}
