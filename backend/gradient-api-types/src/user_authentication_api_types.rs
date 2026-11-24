/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct MakeLoginRequest {
    pub loginname: String,
    pub password: String,
    #[serde(default)]
    pub remember_me: bool,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct MakeUserRequest {
    pub username: String,
    pub name: UserDisplayName,
    pub email: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct CheckUsernameRequest {
    pub username: String,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct CliDeviceStartResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// Deprecated: same as `verification_uri`, with no code. The code in the link allowed an attacker to send a victim a link. One click approved the attacker's own sign-in. Browsers now always require the terminal code.
    #[schema(deprecated)]
    pub verification_uri_complete: String,
    pub expires_in: i64,
    pub interval: u64,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct CliDevicePollRequest {
    pub device_code: String,
}
