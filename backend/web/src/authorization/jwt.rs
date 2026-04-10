/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::State;
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use core::types::input::load_secret;
use core::types::*;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, TokenData, Validation, decode, encode};
use rand::Rng;
use rand::distr::Alphanumeric;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub struct Cliams {
    pub exp: usize,
    pub iat: usize,
    pub id: Uuid,
}

/// Claims for short-lived (1 h) per-build download tokens.
#[derive(Clone, Serialize, Deserialize)]
pub struct DownloadClaims {
    pub exp: usize,
    pub iat: usize,
    pub build_id: Uuid,
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

pub fn encode_jwt(
    state: State<Arc<ServerState>>,
    id: Uuid,
    remember_me: bool,
) -> Result<String, StatusCode> {
    let now = Utc::now();
    let expire: chrono::TimeDelta = if remember_me {
        Duration::days(30) // 30 days for remember me
    } else {
        Duration::hours(24) // 24 hours for regular login
    };
    let exp: usize = (now + expire).timestamp() as usize;
    let iat: usize = now.timestamp() as usize;

    let claim = Cliams { iat, exp, id };
    let secret = load_secret(&state.cli.jwt_secret_file);

    encode(
        &Header::default(),
        &claim,
        &EncodingKey::from_secret(secret.as_ref()),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub async fn decode_jwt(
    state: State<Arc<ServerState>>,
    jwt: String,
) -> Result<TokenData<Cliams>, StatusCode> {
    let result = if jwt.starts_with("GRAD") {
        let api_key = EApi::find()
            .filter(CApi::Key.eq(jwt.strip_prefix("GRAD").ok_or(StatusCode::UNAUTHORIZED)?))
            .one(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        let api_key = match api_key {
            Some(api_key) => api_key,
            None => return Err(StatusCode::UNAUTHORIZED),
        };

        let mut aapi_key: AApi = api_key.clone().into();

        aapi_key.last_used_at = Set(Utc::now().naive_utc());
        aapi_key
            .save(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        TokenData {
            claims: Cliams {
                exp: 0,
                iat: api_key.created_at.and_utc().timestamp() as usize,
                id: api_key.owned_by,
            },
            header: Default::default(),
        }
    } else {
        let secret = load_secret(&state.cli.jwt_secret_file);

        decode(
            &jwt,
            &DecodingKey::from_secret(secret.as_ref()),
            &Validation::default(),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };

    Ok(result)
}

pub fn encode_download_token(
    state: State<Arc<ServerState>>,
    build_id: Uuid,
) -> Result<String, StatusCode> {
    let now = Utc::now();
    let exp = (now + Duration::hours(1)).timestamp() as usize;
    let iat = now.timestamp() as usize;
    let claim = DownloadClaims { iat, exp, build_id };
    let secret = load_secret(&state.cli.jwt_secret_file);
    encode(
        &Header::default(),
        &claim,
        &EncodingKey::from_secret(secret.as_ref()),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub async fn decode_download_token(
    state: State<Arc<ServerState>>,
    token: String,
) -> Result<DownloadClaims, StatusCode> {
    let secret = load_secret(&state.cli.jwt_secret_file);
    decode::<DownloadClaims>(
        &token,
        &DecodingKey::from_secret(secret.as_ref()),
        &Validation::default(),
    )
    .map(|d| d.claims)
    .map_err(|_| StatusCode::UNAUTHORIZED)
}

pub fn generate_api_key() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(64)
        .map(char::from)
        .collect()
}
