/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result, bail};
use axum::extract::State;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use gradient_core::types::input::load_secret;
use gradient_core::types::*;
use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, decode_header, encode,
    jwk::JwkSet,
};
use rand::RngExt as _;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use url::Url;

/// State + nonce returned to the caller of [`oidc_login_create`].
///
/// The caller is expected to set `cookie_value` as `oidc_csrf` cookie
/// (HttpOnly, SameSite=Lax, ~10 min) and redirect the user to `auth_url`.
pub struct OidcAuthRequest {
    pub auth_url: Url,
    pub cookie_value: String,
}

/// Claims for the short-lived `oidc_csrf` cookie.
#[derive(Serialize, Deserialize)]
struct CsrfClaims {
    exp: i64,
    iat: i64,
    state: String,
    nonce: String,
}

/// Subset of ID-token claims we trust as identity.
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
}

fn random_url_safe(bytes: usize) -> String {
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
        .context("Failed to fetch OIDC metadata")?
        .json::<serde_json::Value>()
        .await
        .context("Failed to parse OIDC metadata")?;

    Ok(metadata)
}

async fn fetch_jwks(http_client: &reqwest::Client, jwks_uri: &str) -> Result<JwkSet> {
    let jwks: JwkSet = http_client
        .get(jwks_uri)
        .send()
        .await
        .context("Failed to fetch JWKS")?
        .json()
        .await
        .context("Failed to parse JWKS")?;

    Ok(jwks)
}

pub async fn oidc_login_create(state: State<Arc<ServerState>>) -> Result<OidcAuthRequest> {
    let oidc = state
        .config
        .oidc
        .clone()
        .context("OIDC is not enabled or not fully configured")?;

    let metadata = get_oidc_metadata(&state.http, &oidc.discovery_url).await?;

    let auth_endpoint = metadata["authorization_endpoint"]
        .as_str()
        .context("No authorization_endpoint in OIDC metadata")?;

    let redirect_uri = format!(
        "{}/api/v1/auth/oidc/callback",
        state.config.server.serve_url
    );
    let scope = oidc
        .scopes
        .as_deref()
        .unwrap_or("openid email profile")
        .to_string();

    let csrf_state = random_url_safe(32);
    let nonce = random_url_safe(32);

    let now = Utc::now();
    let claims = CsrfClaims {
        iat: now.timestamp(),
        exp: (now + Duration::minutes(10)).timestamp(),
        state: csrf_state.clone(),
        nonce: nonce.clone(),
    };
    let cookie_value = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(state.jwt_secret.expose().as_bytes()),
    )
    .context("Failed to sign OIDC CSRF cookie")?;

    let params = vec![
        ("response_type", "code"),
        ("client_id", oidc.client_id.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("scope", scope.as_str()),
        ("state", csrf_state.as_str()),
        ("nonce", nonce.as_str()),
    ];

    let auth_url = Url::parse_with_params(auth_endpoint, &params)
        .context("Failed to build authorization URL")?;

    Ok(OidcAuthRequest {
        auth_url,
        cookie_value,
    })
}

pub async fn oidc_login_verify(
    state: State<Arc<ServerState>>,
    authorization_code: String,
    state_query: String,
    csrf_cookie: String,
) -> Result<MUser> {
    let oidc = state
        .config
        .oidc
        .clone()
        .context("OIDC is not enabled or not fully configured")?;

    let csrf_data = decode::<CsrfClaims>(
        &csrf_cookie,
        &DecodingKey::from_secret(state.jwt_secret.expose().as_bytes()),
        &Validation::new(Algorithm::HS256),
    )
    .context("Invalid or expired OIDC CSRF cookie")?;

    if csrf_data
        .claims
        .state
        .as_bytes()
        .ct_eq(state_query.as_bytes())
        .unwrap_u8()
        == 0
    {
        bail!("OIDC state mismatch");
    }

    let expected_nonce = csrf_data.claims.nonce;

    let metadata = get_oidc_metadata(&state.http, &oidc.discovery_url).await?;

    let issuer = metadata["issuer"]
        .as_str()
        .context("No issuer in OIDC metadata")?
        .to_string();
    let token_endpoint = metadata["token_endpoint"]
        .as_str()
        .context("No token_endpoint in OIDC metadata")?;
    let jwks_uri = metadata["jwks_uri"]
        .as_str()
        .context("No jwks_uri in OIDC metadata")?;

    let http_client = &state.http;

    let redirect_uri = format!(
        "{}/api/v1/auth/oidc/callback",
        state.config.server.serve_url
    );
    let client_secret =
        load_secret(&oidc.client_secret_file).context("Failed to read OIDC client secret")?;

    let token_response = http_client
        .post(token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", authorization_code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", oidc.client_id.as_str()),
            ("client_secret", client_secret.expose()),
        ])
        .send()
        .await
        .context("Token exchange request failed")?;

    if !token_response.status().is_success() {
        bail!(
            "Token exchange failed with status {}",
            token_response.status()
        );
    }

    let token_data: serde_json::Value = token_response
        .json::<serde_json::Value>()
        .await
        .context("Failed to parse token response")?;

    let id_token = token_data["id_token"]
        .as_str()
        .context("No id_token in token response")?;

    let claims = verify_id_token(&state.http, id_token, jwks_uri, &issuer, &oidc.client_id).await?;

    match claims.nonce.as_deref() {
        Some(n) if n.as_bytes().ct_eq(expected_nonce.as_bytes()).unwrap_u8() == 1 => {}
        _ => bail!("OIDC nonce mismatch"),
    }

    create_or_update_user(state, claims).await
}

async fn verify_id_token(
    http: &reqwest::Client,
    id_token: &str,
    jwks_uri: &str,
    expected_iss: &str,
    expected_aud: &str,
) -> Result<IdTokenClaims> {
    let header = decode_header(id_token).context("Failed to decode id_token header")?;
    let kid = header.kid.clone().context("id_token header has no kid")?;

    let jwks = fetch_jwks(http, jwks_uri).await?;
    let jwk = jwks.find(&kid).context("No JWK matches id_token kid")?;

    let decoding_key =
        DecodingKey::from_jwk(jwk).context("Failed to construct decoding key from JWK")?;

    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[expected_iss]);
    validation.set_audience(&[expected_aud]);
    validation.validate_exp = true;

    let data = decode::<IdTokenClaims>(id_token, &decoding_key, &validation)
        .context("id_token signature or claims invalid")?;

    Ok(data.claims)
}

async fn create_or_update_user(
    state: State<Arc<ServerState>>,
    claims: IdTokenClaims,
) -> Result<MUser> {
    let display_name = claims.name.clone().unwrap_or_else(|| claims.sub.clone());
    let email = claims.email.clone().unwrap_or_default();
    let login_name = claims
        .preferred_username
        .clone()
        .unwrap_or_else(|| claims.sub.clone());

    let tx = state
        .web_db
        .inner()
        .begin()
        .await
        .context("Failed to start OIDC user transaction")?;

    let existing = EUser::find()
        .filter(CUser::OidcIssuer.eq(&claims.iss))
        .filter(CUser::OidcSubject.eq(&claims.sub))
        .one(&tx)
        .await
        .context("Database error while finding OIDC user")?;

    if let Some(user) = existing {
        let email_changed = claims.email.as_ref().is_some_and(|e| e != &user.email);
        let name_changed = claims.name.as_ref().is_some_and(|n| n != &user.name);

        let mut auser: AUser = user.into();
        auser.last_login_at = Set(gradient_core::types::now());
        if let Some(ref new_email) = claims.email
            && email_changed
        {
            auser.email = Set(new_email.clone());
        }
        if let Some(ref new_name) = claims.name
            && name_changed
        {
            auser.name = Set(new_name.clone());
        }
        let user = auser
            .update(&tx)
            .await
            .context("Failed to update OIDC user")?;

        tx.commit()
            .await
            .context("Failed to commit OIDC user transaction")?;
        return Ok(user);
    }

    let collision = EUser::find()
        .filter(
            sea_orm::Condition::any()
                .add(CUser::Username.eq(&login_name))
                .add(CUser::Email.eq(&email)),
        )
        .one(&tx)
        .await
        .context("Database error while checking for OIDC username/email collision")?;

    if collision.is_some() {
        bail!(
            "An account already exists with this username or email — \
             contact an administrator to link it to your OIDC identity"
        );
    }

    let user = AUser {
        id: Set(UserId::now_v7()),
        username: Set(login_name),
        name: Set(display_name),
        email: Set(email),
        password: Set(None),
        last_login_at: Set(gradient_core::types::now()),
        created_at: Set(gradient_core::types::now()),
        email_verified: Set(true),
        email_verification_token: Set(None),
        email_verification_token_expires: Set(None),
        managed: Set(false),
        superuser: Set(false),
        oidc_issuer: Set(Some(claims.iss)),
        oidc_subject: Set(Some(claims.sub)),
    }
    .insert(&tx)
    .await
    .context("Failed to create user")?;

    tx.commit()
        .await
        .context("Failed to commit OIDC user transaction")?;

    Ok(user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header};

    fn jwt_with_secret(claims: &CsrfClaims, secret: &str) -> String {
        encode(
            &Header::default(),
            claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    #[test]
    fn random_url_safe_is_unique_and_url_safe() {
        let a = random_url_safe(32);
        let b = random_url_safe(32);
        assert_ne!(a, b);
        for c in a.chars() {
            assert!(c.is_ascii_alphanumeric() || c == '-' || c == '_');
        }
    }

    #[test]
    fn csrf_cookie_roundtrips() {
        let now = Utc::now();
        let claims = CsrfClaims {
            iat: now.timestamp(),
            exp: (now + Duration::minutes(5)).timestamp(),
            state: "abc".into(),
            nonce: "xyz".into(),
        };
        let token = jwt_with_secret(&claims, "shh");
        let decoded = decode::<CsrfClaims>(
            &token,
            &DecodingKey::from_secret(b"shh"),
            &Validation::new(Algorithm::HS256),
        )
        .unwrap();
        assert_eq!(decoded.claims.state, "abc");
        assert_eq!(decoded.claims.nonce, "xyz");
    }

    #[test]
    fn csrf_cookie_rejects_wrong_secret() {
        let now = Utc::now();
        let claims = CsrfClaims {
            iat: now.timestamp(),
            exp: (now + Duration::minutes(5)).timestamp(),
            state: "abc".into(),
            nonce: "xyz".into(),
        };
        let token = jwt_with_secret(&claims, "secret-a");
        let decoded = decode::<CsrfClaims>(
            &token,
            &DecodingKey::from_secret(b"secret-b"),
            &Validation::new(Algorithm::HS256),
        );
        assert!(decoded.is_err());
    }

    #[test]
    fn csrf_cookie_rejects_expired() {
        let past = Utc::now() - Duration::hours(1);
        let claims = CsrfClaims {
            iat: past.timestamp() - 60,
            exp: past.timestamp(),
            state: "abc".into(),
            nonce: "xyz".into(),
        };
        let token = jwt_with_secret(&claims, "shh");
        let decoded = decode::<CsrfClaims>(
            &token,
            &DecodingKey::from_secret(b"shh"),
            &Validation::new(Algorithm::HS256),
        );
        assert!(decoded.is_err());
    }

    #[test]
    fn state_compare_constant_time_rejects_mismatch() {
        let a = "abc".as_bytes();
        let b = "abd".as_bytes();
        assert_eq!(a.ct_eq(b).unwrap_u8(), 0);
        assert_eq!(a.ct_eq(a).unwrap_u8(), 1);
    }
}
