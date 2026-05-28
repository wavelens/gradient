use crate::{Client, ConnectorError, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

/// Outcome of `/auth/cli/poll`. Pending/Expired/Denied are normal states of the
/// device flow, not transport errors, so the CLI matches on them instead of
/// reading prose out of `ConnectorError::Api`.
#[derive(Debug, Clone)]
pub enum CliPollOutcome {
    Pending,
    Expired,
    Denied,
    Token(String),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MakeUserRequest {
    pub username: String,
    pub name: String,
    pub email: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MakeLoginRequest {
    pub loginname: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VerifyEmailRequest {
    pub token: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ResendRequest {
    pub username: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OidcLoginRequest {
    pub redirect_uri: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OidcRedirect {
    pub url: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CliDeviceStartResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: i64,
    pub interval: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CliDevicePollRequest {
    pub device_code: String,
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

    pub async fn check_username(&self, username: &str) -> Result<bool, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("auth/check-username?username={}", username),
            false,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn verify_email(&self, body: VerifyEmailRequest) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            "auth/verify-email",
            false,
        )?
        .json(&body);
        http::decode(req.send().await?).await
    }

    pub async fn resend_verification(&self, body: ResendRequest) -> Result<String, ConnectorError> {
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

    pub async fn oidc_login(&self, body: OidcLoginRequest) -> Result<OidcRedirect, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            "auth/oidc/login",
            false,
        )?
        .json(&body);
        http::decode(req.send().await?).await
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
