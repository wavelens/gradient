/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::jwt::{sign_claims, verify_claims};
use super::oidc::random_url_safe;
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use gradient_core::{AuthorizeRequest, CodeExchange, ExternalIdentity, LoginProvider};
use gradient_types::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderPurpose {
    Login,
    Connect {
        user_id: UserId,
        session_id: SessionId,
    },
}

pub struct ProviderAuthRequest {
    pub auth_url: Url,
    pub cookie_value: String,
}

pub struct ProviderCallback<'a> {
    pub code: &'a str,
    pub state: &'a str,
    pub csrf_cookie: &'a str,
}

#[derive(Serialize, Deserialize)]
struct CsrfClaims {
    exp: i64,
    iat: i64,
    provider: String,
    state: String,
    pkce_verifier: String,
    purpose: ProviderPurpose,
}

pub fn provider_redirect_uri(config: &RuntimeConfig, provider: &dyn LoginProvider) -> String {
    format!(
        "{}/api/v1/auth/providers/{}/callback",
        config.server.serve_url,
        provider.id()
    )
}

pub fn provider_authorize(
    config: &RuntimeConfig,
    jwt_secret: &SecretString,
    provider: &dyn LoginProvider,
    purpose: ProviderPurpose,
) -> Result<ProviderAuthRequest> {
    let csrf_state = random_url_safe(32);
    let pkce_verifier = random_url_safe(32);
    let pkce_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce_verifier.as_bytes()));

    let now = Utc::now();
    let claims = CsrfClaims {
        iat: now.timestamp(),
        exp: (now + Duration::minutes(10)).timestamp(),
        provider: provider.id().to_owned(),
        state: csrf_state.clone(),
        pkce_verifier,
        purpose,
    };
    let cookie_value = sign_claims(jwt_secret, &claims).context("sign login CSRF cookie")?;

    let redirect_uri = provider_redirect_uri(config, provider);
    let auth_url = provider.authorize_url(AuthorizeRequest {
        redirect_uri: &redirect_uri,
        state: &csrf_state,
        pkce_challenge: &pkce_challenge,
    })?;

    Ok(ProviderAuthRequest {
        auth_url,
        cookie_value,
    })
}

pub async fn provider_callback(
    config: &RuntimeConfig,
    http: &reqwest::Client,
    jwt_secret: &SecretString,
    provider: &dyn LoginProvider,
    callback: ProviderCallback<'_>,
) -> Result<(ProviderPurpose, ExternalIdentity)> {
    let csrf: CsrfClaims = verify_claims(jwt_secret, callback.csrf_cookie)
        .context("invalid or expired login CSRF cookie")?;
    if csrf.provider != provider.id() {
        bail!("login CSRF cookie belongs to another provider");
    }
    if csrf
        .state
        .as_bytes()
        .ct_eq(callback.state.as_bytes())
        .unwrap_u8()
        == 0
    {
        bail!("login state mismatch");
    }

    let redirect_uri = provider_redirect_uri(config, provider);
    let identity = provider
        .identity(
            http,
            CodeExchange {
                code: callback.code,
                redirect_uri: &redirect_uri,
                pkce_verifier: &csrf.pkce_verifier,
            },
        )
        .await?;
    if identity.issuer != provider.issuer() {
        bail!("login provider answered with a foreign issuer");
    }
    Ok((csrf.purpose, identity))
}
