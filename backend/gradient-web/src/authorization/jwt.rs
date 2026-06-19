/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::api_key::{ApiKeyContext, DecodedRequest};
use axum::extract::State;
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use gradient_types::*;
use gradient_core::ServerState;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rand::distr::{Alphanumeric, SampleString};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Session-backed JWT claims. `jti` is the `SessionId` of a row in the
/// `session` table; auth lookups validate the session is non-revoked and
/// non-expired before trusting the token.
#[derive(Clone, Serialize, Deserialize)]
pub struct Cliams {
    pub exp: usize,
    pub iat: usize,
    pub id: UserId,
    /// Session id - must match a non-revoked, non-expired row in `session`.
    pub jti: SessionId,
}

/// Claims for short-lived (1 h) per-build download tokens. Scoped to a
/// `derivation` (outputs are resolved through it) and the originating
/// `evaluation` for access attribution.
#[derive(Clone, Serialize, Deserialize)]
pub struct DownloadClaims {
    pub exp: usize,
    pub iat: usize,
    pub derivation: DerivationId,
    pub evaluation: EvaluationId,
}

pub(super) fn token_from_cookie(req: &axum::extract::Request) -> Option<String> {
    let cookie_header = req
        .headers()
        .get(axum::http::header::COOKIE)?
        .to_str()
        .ok()?;
    cookie_header
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("jwt_token=").map(str::to_owned))
}

/// Extract a bearer token from the Authorization header or the `jwt_token` cookie.
pub fn extract_bearer_or_cookie(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(auth) = headers.get(axum::http::header::AUTHORIZATION) {
        let val = auth.to_str().ok()?;
        let mut parts = val.split_whitespace();
        if parts.next() == Some("Bearer") {
            return parts.next().map(str::to_owned);
        }
    }
    let cookie_header = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    cookie_header
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("jwt_token=").map(str::to_owned))
}

/// Persist a new `session` row for `user_id` and encode a JWT carrying its id
/// as the `jti` claim. Logout (or any explicit revoke) invalidates the row,
/// which the auth middleware checks on every request.
pub async fn create_session_and_token(
    state: State<Arc<ServerState>>,
    user_id: UserId,
    remember_me: bool,
    user_agent: Option<String>,
    ip: Option<String>,
) -> Result<(SessionId, String), StatusCode> {
    let now = Utc::now();
    let lifetime = if remember_me {
        Duration::days(30)
    } else {
        Duration::hours(24)
    };
    let expires_at = now + lifetime;

    let session_id = SessionId::now_v7();
    let session = MSession {
        id: session_id,
        user_id,
        created_at: now.naive_utc(),
        expires_at: expires_at.naive_utc(),
        last_used_at: now.naive_utc(),
        user_agent,
        ip,
        remember_me,
        ..Default::default()
    }
    .into_active_model();

    session
        .insert(&state.web_db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let claim = Cliams {
        iat: now.timestamp() as usize,
        exp: expires_at.timestamp() as usize,
        id: user_id,
        jti: session_id,
    };
    let token = encode(
        &Header::default(),
        &claim,
        &EncodingKey::from_secret(state.jwt_secret.expose().as_bytes()),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((session_id, token))
}

/// Decode a JWT or API-key token. For session JWTs, validates the matching
/// session row exists, is not revoked, and is not expired.
pub async fn decode_jwt(
    state: State<Arc<ServerState>>,
    jwt: String,
) -> Result<DecodedRequest, StatusCode> {
    if let Some(raw) = jwt.strip_prefix("GRAD") {
        return decode_api_key(state, raw).await;
    }

    let token_data = decode::<Cliams>(
        &jwt,
        &DecodingKey::from_secret(state.jwt_secret.expose().as_bytes()),
        &Validation::default(),
    )
    .map_err(|_| StatusCode::UNAUTHORIZED)?;

    let session = ESession::find_by_id(token_data.claims.jti)
        .one(&state.web_db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let now = gradient_types::now();
    if session.revoked_at.is_some()
        || session.expires_at < now
        || session.user_id != token_data.claims.id
    {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let mut active: ASession = session.into();
    active.last_used_at = Set(now);
    let _ = active.update(&state.web_db).await;

    Ok(DecodedRequest::Session {
        user_id: token_data.claims.id,
    })
}

async fn decode_api_key(
    state: State<Arc<ServerState>>,
    raw: &str,
) -> Result<DecodedRequest, StatusCode> {
    let key_hash = hash_api_key(raw);
    let api_key = EApi::find()
        .filter(CApi::Key.eq(key_hash))
        .one(&state.web_db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let now = gradient_types::now();
    if api_key.revoked_at.is_some() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if let Some(exp) = api_key.expires_at
        && exp < now
    {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let context = ApiKeyContext {
        api_id: api_key.id,
        mask: api_key.permission,
        organization: api_key.organization,
        cache_pin: api_key.cache,
        cache_permission_mask: if api_key.cache.is_some() {
            Some(api_key.permission)
        } else {
            None
        },
        allowed_ips: api_key.allowed_ips.clone().unwrap_or_default(),
    };
    let user_id = api_key.owned_by;

    let mut aapi_key: AApi = api_key.into();
    aapi_key.last_used_at = Set(now);
    aapi_key
        .save(&state.web_db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(DecodedRequest::ApiKey { user_id, context })
}

pub fn encode_download_token(
    state: State<Arc<ServerState>>,
    derivation: DerivationId,
    evaluation: EvaluationId,
) -> Result<String, StatusCode> {
    let now = Utc::now();
    let exp = (now + Duration::hours(1)).timestamp() as usize;
    let iat = now.timestamp() as usize;
    let claim = DownloadClaims { iat, exp, derivation, evaluation };
    encode(
        &Header::default(),
        &claim,
        &EncodingKey::from_secret(state.jwt_secret.expose().as_bytes()),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub async fn decode_download_token(
    state: State<Arc<ServerState>>,
    token: String,
) -> Result<DownloadClaims, StatusCode> {
    decode::<DownloadClaims>(
        &token,
        &DecodingKey::from_secret(state.jwt_secret.expose().as_bytes()),
        &Validation::default(),
    )
    .map(|d| d.claims)
    .map_err(|_| StatusCode::UNAUTHORIZED)
}

pub fn generate_api_key() -> String {
    Alphanumeric.sample_string(&mut rand::rng(), 64)
}

/// Lowercase hex SHA-256 of the raw token (the part after the `GRAD` prefix).
/// API keys are stored hashed; this is also what `state.api_keys[*].key_file`
/// must contain.
pub fn hash_api_key(raw: &str) -> String {
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    let bytes = h.finalize();
    let mut out = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write as _;
        write!(&mut out, "{:02x}", b).unwrap();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue, header};

    fn headers_with(name: header::HeaderName, value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(name, HeaderValue::from_str(value).unwrap());
        h
    }

    #[test]
    fn extract_bearer_from_authorization_header() {
        let h = headers_with(header::AUTHORIZATION, "Bearer abc.def.ghi");
        assert_eq!(extract_bearer_or_cookie(&h).as_deref(), Some("abc.def.ghi"));
    }

    #[test]
    fn extract_bearer_scheme_is_case_sensitive() {
        let h = headers_with(header::AUTHORIZATION, "bearer abc");
        assert_eq!(extract_bearer_or_cookie(&h), None);
    }

    #[test]
    fn extract_basic_auth_is_ignored() {
        let h = headers_with(header::AUTHORIZATION, "Basic dXNlcjpwYXNz");
        assert_eq!(extract_bearer_or_cookie(&h), None);
    }

    #[test]
    fn extract_falls_back_to_cookie_when_no_auth_header() {
        let h = headers_with(header::COOKIE, "other=1; jwt_token=xyz; last=2");
        assert_eq!(extract_bearer_or_cookie(&h).as_deref(), Some("xyz"));
    }

    #[test]
    fn extract_cookie_without_jwt_token_returns_none() {
        let h = headers_with(header::COOKIE, "other=1; session=2");
        assert_eq!(extract_bearer_or_cookie(&h), None);
    }

    #[test]
    fn extract_empty_headers_returns_none() {
        assert_eq!(extract_bearer_or_cookie(&HeaderMap::new()), None);
    }

    #[test]
    fn extract_bearer_with_no_token_part_returns_none() {
        let h = headers_with(header::AUTHORIZATION, "Bearer");
        assert_eq!(extract_bearer_or_cookie(&h), None);
    }
}
