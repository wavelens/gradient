/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::WebResult;
use crate::refusal::{Refusal, RefusalStatus};
use base64::Engine as _;
use rand::RngExt as _;

#[derive(Debug, thiserror::Error)]
enum WorkerTokenRefusal {
    #[error("token is not valid base64")]
    InvalidBase64,
    #[error("token must be 48 raw bytes encoded as base64 (openssl rand -base64 48)")]
    WrongTokenLength,
}

impl Refusal for WorkerTokenRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::InvalidBase64 | Self::WrongTokenLength => RefusalStatus::BadRequest,
        }
    }
}

pub fn issue(provided: Option<String>) -> WebResult<(String, bool)> {
    if let Some(provided) = provided {
        let token = provided.trim().to_string();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&token)
            .map_err(|_| WorkerTokenRefusal::InvalidBase64)?;
        if decoded.len() != 48 {
            return Err(WorkerTokenRefusal::WrongTokenLength.into());
        }
        return Ok((token, false));
    }

    let mut raw = [0u8; 48];
    rand::rng().fill(&mut raw);
    Ok((base64::engine::general_purpose::STANDARD.encode(raw), true))
}
