/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::refusal::{Refusal, RefusalStatus};

#[derive(Debug, thiserror::Error)]
pub(crate) enum WebhookRefusal {
    #[error("{0}")]
    InvalidEventFilter(String),
    #[error("{0}")]
    InvalidUrl(String),
    #[error("login required")]
    LoginRequired,
}

impl Refusal for WebhookRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::InvalidEventFilter(_) | Self::InvalidUrl(_) => RefusalStatus::UnprocessableEntity,
            Self::LoginRequired => RefusalStatus::Unauthorized,
        }
    }
}
