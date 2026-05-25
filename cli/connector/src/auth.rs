use crate::{Client, ConnectorError, http};
use reqwest::Method;
use serde::{Deserialize, Serialize};

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
