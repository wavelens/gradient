/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::api_key::{ApiKeyContext, DecodedRequest};
use axum::http::{HeaderMap, StatusCode, header};
use chrono::{Duration, Utc};
use gradient_db::WebDb;
use gradient_db::lookup::by_id;
use gradient_types::*;
use gradient_util::debounce::Debounce;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rand::distr::{Alphanumeric, SampleString};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Instant;
use tracing::warn;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub struct Cliams {
    pub exp: usize,
    pub iat: usize,
    pub id: UserId,
    pub jti: SessionId,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DownloadClaims {
    pub exp: usize,
    pub iat: usize,
    pub derivation: DerivationId,
    pub evaluation: EvaluationId,
}

pub fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix(name)?.strip_prefix('='))
}

pub(super) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let mut parts = headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .split_whitespace();

    parts.next().filter(|scheme| *scheme == "Bearer")?;
    parts.next()
}

pub(super) fn session_cookie_token(headers: &HeaderMap) -> Option<&str> {
    cookie_value(headers, "jwt_token")
}

pub fn extract_bearer_or_cookie(headers: &HeaderMap) -> Option<&str> {
    bearer_token(headers).or_else(|| session_cookie_token(headers))
}

pub(super) fn sign_claims<T: Serialize>(
    jwt_secret: &SecretString,
    claims: &T,
) -> jsonwebtoken::errors::Result<String> {
    encode(
        &Header::default(),
        claims,
        &EncodingKey::from_secret(jwt_secret.expose().as_bytes()),
    )
}

pub(super) fn verify_claims<T: DeserializeOwned>(
    jwt_secret: &SecretString,
    token: &str,
) -> jsonwebtoken::errors::Result<T> {
    decode::<T>(
        token,
        &DecodingKey::from_secret(jwt_secret.expose().as_bytes()),
        &Validation::default(),
    )
    .map(|data| data.claims)
}

pub fn session_claims(jwt_secret: &SecretString, headers: &HeaderMap) -> Option<Cliams> {
    verify_claims(jwt_secret, extract_bearer_or_cookie(headers)?).ok()
}

pub async fn create_session_and_token(
    db: &WebDb,
    jwt_secret: &SecretString,
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
        .insert(db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let claim = Cliams {
        iat: now.timestamp() as usize,
        exp: expires_at.timestamp() as usize,
        id: user_id,
        jti: session_id,
    };

    let token = sign_claims(jwt_secret, &claim).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((session_id, token))
}

pub async fn decode_jwt(
    db: &WebDb,
    jwt_secret: &SecretString,
    last_used_stamps: &Debounce<Uuid>,
    jwt: String,
) -> Result<DecodedRequest, StatusCode> {
    if let Some(raw) = jwt.strip_prefix("GRAD") {
        return decode_api_key(db, last_used_stamps, raw).await;
    }

    let claims: Cliams = verify_claims(jwt_secret, &jwt).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let session = by_id::<ESession, _>(db, claims.jti)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let now = gradient_types::now();
    if !session.is_usable_at(now) || session.user_id != claims.id {
        return Err(StatusCode::UNAUTHORIZED);
    }

    if last_used_stamps.due(session.id.into_inner(), Instant::now()) {
        let mut active: ASession = session.into();
        active.last_used_at = Set(now);

        if let Err(e) = active.update(db).await {
            warn!(error = %e, "session last_used_at stamp failed");
        }
    }

    Ok(DecodedRequest::Session { user_id: claims.id })
}

async fn decode_api_key(
    db: &WebDb,
    last_used_stamps: &Debounce<Uuid>,
    raw: &str,
) -> Result<DecodedRequest, StatusCode> {
    let key_hash = hash_api_key(raw);
    let api_key = EApi::find()
        .filter(CApi::Key.eq(key_hash))
        .one(db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let now = gradient_types::now();
    if !api_key.is_usable_at(now) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let context = ApiKeyContext {
        api_id: api_key.id,
        mask: api_key.permission,
        project: api_key.project,
        cache_pin: api_key.cache,
        cache_permission_mask: if api_key.cache.is_some() {
            Some(api_key.permission)
        } else {
            None
        },
        allowed_ips: api_key.allowed_ips.clone().unwrap_or_default(),
    };

    let user_id = api_key.owned_by;

    if last_used_stamps.due(api_key.id.into_inner(), Instant::now()) {
        let mut active: AApi = api_key.into();
        active.last_used_at = Set(now);

        if let Err(e) = active.save(db).await {
            warn!(error = %e, "api key last_used_at stamp failed");
        }
    }

    Ok(DecodedRequest::ApiKey { user_id, context })
}

pub fn encode_download_token(
    jwt_secret: &SecretString,
    derivation: DerivationId,
    evaluation: EvaluationId,
) -> Result<String, StatusCode> {
    let now = Utc::now();
    let exp = (now + Duration::hours(1)).timestamp() as usize;
    let iat = now.timestamp() as usize;
    let claim = DownloadClaims {
        iat,
        exp,
        derivation,
        evaluation,
    };

    sign_claims(jwt_secret, &claim).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub async fn decode_download_token(
    jwt_secret: &SecretString,
    token: String,
) -> Result<DownloadClaims, StatusCode> {
    verify_claims(jwt_secret, &token).map_err(|_| StatusCode::UNAUTHORIZED)
}

pub fn generate_api_key() -> String {
    Alphanumeric.sample_string(&mut rand::rng(), 64)
}

pub fn hash_api_key(raw: &str) -> String {
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use axum::http::header::{AUTHORIZATION, COOKIE, HeaderName};

    type Case<'a> = (&'a str, &'a [(HeaderName, &'a str)], Option<&'a str>);

    #[test]
    fn extract_bearer_or_cookie_reads_the_presented_token() {
        let cases: [Case<'_>; 9] = [
            (
                "bearer token",
                &[(AUTHORIZATION, "Bearer abc.def.ghi")],
                Some("abc.def.ghi"),
            ),
            (
                "scheme is case sensitive",
                &[(AUTHORIZATION, "bearer abc")],
                None,
            ),
            (
                "basic scheme is ignored",
                &[(AUTHORIZATION, "Basic dXNlcjpwYXNz")],
                None,
            ),
            ("bearer without a token", &[(AUTHORIZATION, "Bearer")], None),
            (
                "session cookie among others",
                &[(COOKIE, "other=1; jwt_token=xyz; last=2")],
                Some("xyz"),
            ),
            (
                "cookie whose name only starts alike",
                &[(COOKIE, "jwt_token_old=1")],
                None,
            ),
            (
                "cookies without the session cookie",
                &[(COOKIE, "other=1; session=2")],
                None,
            ),
            (
                "bearer wins over the session cookie",
                &[(AUTHORIZATION, "Bearer abc"), (COOKIE, "jwt_token=xyz")],
                Some("abc"),
            ),
            ("no headers", &[], None),
        ];

        for (case, entries, expected) in cases {
            let headers: HeaderMap = entries
                .iter()
                .map(|(name, value)| (name.clone(), HeaderValue::from_str(value).unwrap()))
                .collect();

            assert_eq!(extract_bearer_or_cookie(&headers), expected, "{case}");
        }
    }
}
