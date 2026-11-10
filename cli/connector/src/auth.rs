// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    CheckUsernameRequest, CliDevicePollRequest, CliDeviceStartResponse, MakeLoginRequest,
    MakeUserRequest,
};
use crate::{Client, ConnectorError, http};
use reqwest::Method;

#[derive(Debug, Clone)]
pub enum CliPollOutcome {
    Pending,
    Expired,
    Denied,
    Token(String),
}

pub struct AuthApi<'a>(pub(crate) &'a Client);

impl AuthApi<'_> {
    pub async fn register(&self, body: MakeUserRequest) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            "auth/basic/register",
            false,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn basic_login(&self, body: MakeLoginRequest) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            "auth/basic/login",
            false,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn resend_verification(
        &self,
        body: CheckUsernameRequest,
    ) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            "auth/resend-verification",
            false,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn renew_session(&self) -> Result<String, ConnectorError> {
        self.0
            .send_authenticated_request(Method::POST, "auth/session/renew", None::<&()>)
            .await
    }

    pub async fn cli_device_start(&self) -> Result<CliDeviceStartResponse, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            "auth/cli/start",
            false,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn cli_device_poll(
        &self,
        body: CliDevicePollRequest,
    ) -> Result<CliPollOutcome, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            "auth/cli/poll",
            false,
        )?
        .json(&body);
        http::decode_cli_poll(req.send().await?).await
    }

    pub async fn logout(&self) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            "auth/logout",
            true,
        )?;
        http::decode(req.send().await?).await
    }
}
