/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use base64::Engine as _;
use rand::RngExt as _;

pub fn issue(provided: Option<String>) -> WebResult<(String, bool)> {
    if let Some(provided) = provided {
        let token = provided.trim().to_string();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&token)
            .map_err(|_| WebError::bad_request("token is not valid base64"))?;
        if decoded.len() != 48 {
            return Err(WebError::bad_request(
                "token must be 48 raw bytes encoded as base64 (openssl rand -base64 48)",
            ));
        }
        return Ok((token, false));
    }

    let mut raw = [0u8; 48];
    rand::rng().fill(&mut raw);
    Ok((base64::engine::general_purpose::STANDARD.encode(raw), true))
}
