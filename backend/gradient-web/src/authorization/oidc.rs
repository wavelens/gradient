/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::external_login::{PasswordLogin, sign_in_external};
use super::jwt::{sign_claims, verify_claims};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use gradient_core::ServerState;
use gradient_core::{ExternalIdentity, MatchRule};
use gradient_types::input::load_secret;
use gradient_types::*;
use jsonwebtoken::{DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use rand::RngExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;

pub struct OidcAuthRequest {
    pub auth_url: Url,
    pub cookie_value: String,
}

#[derive(Serialize, Deserialize)]
struct CsrfClaims {
    exp: i64,
    iat: i64,
    state: String,
    nonce: String,
    pkce_verifier: String,
}

#[derive(Deserialize)]
struct IdTokenClaims {
    iss: String,
    sub: String,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    preferred_username: Option<String>,
    #[serde(default)]
    groups: Vec<String>,
}

pub(super) fn random_url_safe(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill(buf.as_mut_slice());
    URL_SAFE_NO_PAD.encode(buf)
}

async fn get_oidc_metadata(
    http_client: &reqwest::Client,
    discovery_url: &str,
) -> Result<serde_json::Value> {
    let url = if discovery_url.ends_with("/.well-known/openid-configuration") {
        discovery_url.to_string()
    } else {
        format!(
            "{}/.well-known/openid-configuration",
            discovery_url.trim_end_matches('/')
        )
    };

    let metadata = http_client
        .get(url)
        .send()
        .await
        .context("fetch OIDC metadata")?
        .json::<serde_json::Value>()
        .await
        .context("parse OIDC metadata")?;

    Ok(metadata)
}

async fn fetch_jwks(http_client: &reqwest::Client, jwks_uri: &str) -> Result<JwkSet> {
    let jwks: JwkSet = http_client
        .get(jwks_uri)
        .send()
        .await
        .context("fetch JWKS")?
        .json()
        .await
        .context("parse JWKS")?;

    Ok(jwks)
}

pub async fn oidc_login_create(
    config: &RuntimeConfig,
    http: &reqwest::Client,
    jwt_secret: &SecretString,
) -> Result<OidcAuthRequest> {
    let oidc = config
        .oidc
        .clone()
        .context("OIDC is not enabled or not fully configured")?;

    let metadata = get_oidc_metadata(http, &oidc.discovery_url).await?;

    let auth_endpoint = metadata["authorization_endpoint"]
        .as_str()
        .context("no authorization_endpoint in OIDC metadata")?;

    let redirect_uri = format!("{}/api/v1/auth/oidc/callback", config.server.serve_url);
    let scope = oidc.scopes.join(" ");

    let csrf_state = random_url_safe(32);
    let nonce = random_url_safe(32);
    let pkce_verifier = random_url_safe(32);
    let pkce_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce_verifier.as_bytes()));

    let now = Utc::now();
    let claims = CsrfClaims {
        iat: now.timestamp(),
        exp: (now + Duration::minutes(10)).timestamp(),
        state: csrf_state.clone(),
        nonce: nonce.clone(),
        pkce_verifier,
    };
    let cookie_value = sign_claims(jwt_secret, &claims).context("sign OIDC CSRF cookie")?;

    let params = vec![
        ("response_type", "code"),
        ("client_id", oidc.client_id.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("scope", scope.as_str()),
        ("state", csrf_state.as_str()),
        ("nonce", nonce.as_str()),
        ("code_challenge", pkce_challenge.as_str()),
        ("code_challenge_method", "S256"),
    ];

    let auth_url =
        Url::parse_with_params(auth_endpoint, &params).context("build authorization URL")?;

    Ok(OidcAuthRequest {
        auth_url,
        cookie_value,
    })
}

pub async fn oidc_login_verify(
    state: &ServerState,
    authorization_code: String,
    state_query: String,
    csrf_cookie: String,
) -> Result<MUser> {
    let oidc = state
        .config
        .oidc
        .clone()
        .context("OIDC is not enabled or not fully configured")?;

    let csrf: CsrfClaims = verify_claims(&state.jwt_secret, &csrf_cookie)
        .context("invalid or expired OIDC CSRF cookie")?;

    if csrf
        .state
        .as_bytes()
        .ct_eq(state_query.as_bytes())
        .unwrap_u8()
        == 0
    {
        bail!("OIDC state mismatch");
    }

    let expected_nonce = csrf.nonce;
    let pkce_verifier = csrf.pkce_verifier;

    let metadata = get_oidc_metadata(&state.http, &oidc.discovery_url).await?;

    let issuer = metadata["issuer"]
        .as_str()
        .context("no issuer in OIDC metadata")?
        .to_string();
    let token_endpoint = metadata["token_endpoint"]
        .as_str()
        .context("no token_endpoint in OIDC metadata")?;
    let jwks_uri = metadata["jwks_uri"]
        .as_str()
        .context("no jwks_uri in OIDC metadata")?;

    let http_client = &state.http;

    let redirect_uri = format!(
        "{}/api/v1/auth/oidc/callback",
        state.config.server.serve_url
    );
    let client_secret = load_secret(&oidc.client_secret_file).context("read OIDC client secret")?;

    let token_response = http_client
        .post(token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", authorization_code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", oidc.client_id.as_str()),
            ("client_secret", client_secret.expose()),
            ("code_verifier", pkce_verifier.as_str()),
        ])
        .send()
        .await
        .context("send token exchange request")?;

    if !token_response.status().is_success() {
        bail!(
            "Token exchange failed with status {}",
            token_response.status()
        );
    }

    let token_data: serde_json::Value = token_response
        .json::<serde_json::Value>()
        .await
        .context("parse token response")?;

    let id_token = token_data["id_token"]
        .as_str()
        .context("no id_token in token response")?;

    let claims = verify_id_token(&state.http, id_token, jwks_uri, &issuer, &oidc.client_id).await?;

    match claims.nonce.as_deref() {
        Some(n) if n.as_bytes().ct_eq(expected_nonce.as_bytes()).unwrap_u8() == 1 => {}
        _ => bail!("OIDC nonce mismatch"),
    }

    sign_in_external(
        &state.web_db,
        &state.memory.pending_project_memberships,
        external_identity(claims),
        PasswordLogin::of(&state.login_methods()),
    )
    .await
}

async fn verify_id_token(
    http: &reqwest::Client,
    id_token: &str,
    jwks_uri: &str,
    expected_iss: &str,
    expected_aud: &str,
) -> Result<IdTokenClaims> {
    let header = decode_header(id_token).context("decode id_token header")?;
    let kid = header.kid.clone().context("id_token header has no kid")?;

    let jwks = fetch_jwks(http, jwks_uri).await?;
    let jwk = jwks.find(&kid).context("no JWK matches id_token kid")?;

    let decoding_key = DecodingKey::from_jwk(jwk).context("construct decoding key from JWK")?;

    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[expected_iss]);
    validation.set_audience(&[expected_aud]);
    validation.validate_exp = true;

    let data = decode::<IdTokenClaims>(id_token, &decoding_key, &validation)
        .context("id_token signature or claims invalid")?;

    Ok(data.claims)
}

fn external_identity(claims: IdTokenClaims) -> ExternalIdentity {
    let username = claims
        .preferred_username
        .unwrap_or_else(|| claims.sub.clone());
    ExternalIdentity {
        issuer: claims.iss,
        subject: claims.sub,
        username,
        email: claims.email,
        name: claims.name,
        groups: Some(claims.groups),
        match_rule: MatchRule::UsernameOrEmail,
        refresh_profile: true,
    }
}
