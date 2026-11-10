/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::api_key::{ApiKeyContext, DecodedRequest};
use super::job_scope::job_mask;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use chrono::{Duration, Utc};
use gradient_db::WebDb;
use gradient_db::lookup::by_id;
use gradient_types::exec::{JOB_TOKEN_PREFIX, hash_job_token};
use gradient_types::*;
use gradient_util::debounce::Debounce;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rand::distr::{Alphanumeric, SampleString};
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

fn unix_seconds(time: chrono::DateTime<Utc>) -> Result<usize, StatusCode> {
    usize::try_from(time.timestamp()).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
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

pub fn clear_jwt_token_cookie(use_tls: bool) -> String {
    let secure = if use_tls { "; Secure" } else { "" };
    format!("jwt_token=; HttpOnly{secure}; SameSite=Strict; Path=/; Max-Age=0")
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

pub fn jwt_cookie(token: &str, remember_me: bool, use_tls: bool) -> String {
    let secure = if use_tls { "; Secure" } else { "" };
    let base = format!("jwt_token={token}; HttpOnly{secure}; SameSite=Strict; Path=/");
    if remember_me {
        let max_age = MSession::sign_in_session_valid_duration(true).num_seconds();
        format!("{base}; Max-Age={max_age}")
    } else {
        base
    }
}

fn sign_session_jwt(jwt_secret: &SecretString, session: &MSession) -> Result<String, StatusCode> {
    let claim = Cliams {
        iat: unix_seconds(Utc::now())?,
        exp: unix_seconds(session.expires_at.and_utc())?,
        id: session.user_id,
        jti: session.id,
    };

    sign_claims(jwt_secret, &claim).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub async fn create_session_and_token(
    db: &WebDb,
    jwt_secret: &SecretString,
    user_id: UserId,
    remember_me: bool,
    user_agent: Option<String>,
    ip: Option<String>,
) -> Result<(SessionId, String), StatusCode> {
    let now = gradient_types::now();
    let session = MSession {
        id: SessionId::now_v7(),
        user_id,
        created_at: now,
        expires_at: now + MSession::sign_in_session_valid_duration(remember_me),
        last_used_at: now,
        user_agent,
        ip,
        remember_me,
        ..Default::default()
    };

    let token = sign_session_jwt(jwt_secret, &session)?;
    let session_id = session.id;
    gradient_db::rows::insert::<ESession, _>(db, session)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((session_id, token))
}

pub(super) async fn extend_session_and_return_set_cookie_if_due(
    db: &WebDb,
    jwt_secret: &SecretString,
    decoded: &DecodedRequest,
    headers: &HeaderMap,
    use_tls: bool,
) -> Option<HeaderValue> {
    let DecodedRequest::Session { session } = decoded else {
        return None;
    };
    let now = gradient_types::now();
    if bearer_token(headers).is_some() || !session.has_passed_half_validity_at(now) {
        return None;
    }

    let session = gradient_db::sessions::extend_expiry(db, session.clone(), now)
        .await
        .inspect_err(|e| warn!(error = %e, "failed to extend session expiry"))
        .ok()?;
    let token = sign_session_jwt(jwt_secret, &session).ok()?;

    HeaderValue::from_str(&jwt_cookie(&token, session.remember_me, use_tls)).ok()
}

pub async fn decode_jwt(
    db: &WebDb,
    jwt_secret: &SecretString,
    last_used_stamps: &Debounce<Uuid>,
    jwt: String,
) -> Result<DecodedRequest, StatusCode> {
    if jwt.starts_with(JOB_TOKEN_PREFIX) {
        return decode_job_token(db, &jwt).await;
    }

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

    if last_used_stamps.due(session.id.into_inner(), Instant::now())
        && let Err(e) = gradient_db::sessions::stamp_last_used(db, session.clone(), now).await
    {
        warn!(error = %e, "session last_used_at stamp failed");
    }

    Ok(DecodedRequest::Session { session })
}

async fn decode_api_key(
    db: &WebDb,
    last_used_stamps: &Debounce<Uuid>,
    raw: &str,
) -> Result<DecodedRequest, StatusCode> {
    let key_hash = hash_api_key(raw);
    let api_key = gradient_db::api_keys::by_key_hash(db, &key_hash)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let now = gradient_types::now();
    if !api_key.is_usable_at(now) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let context = ApiKeyContext {
        mask: api_key.permission,
        project: api_key.project,
        cache_pin: api_key.cache,
        allowed_ips: api_key.allowed_ips.clone().unwrap_or_default(),
    };

    let user_id = api_key.owned_by;
    let key = api_key.id;

    if last_used_stamps.due(api_key.id.into_inner(), Instant::now())
        && let Err(e) = gradient_db::api_keys::stamp_last_used(db, api_key, now).await
    {
        warn!(error = %e, "api key last_used_at stamp failed");
    }

    Ok(DecodedRequest::ApiKey {
        user_id,
        key,
        context,
    })
}

async fn decode_job_token(db: &WebDb, raw: &str) -> Result<DecodedRequest, StatusCode> {
    let job =
        gradient_db::exec_jobs::tokens::resolve(db, &hash_job_token(raw), gradient_types::now())
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::UNAUTHORIZED)?;

    let context = ApiKeyContext {
        mask: job_mask(),
        project: Some(job.project),
        cache_pin: None,
        allowed_ips: Vec::new(),
    };

    Ok(DecodedRequest::Job {
        user_id: job.acting_user,
        context,
        job,
    })
}

pub fn encode_download_token(
    jwt_secret: &SecretString,
    derivation: DerivationId,
    evaluation: EvaluationId,
) -> Result<String, StatusCode> {
    let now = Utc::now();
    let exp = unix_seconds(now + Duration::hours(1))?;
    let iat = unix_seconds(now)?;
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
