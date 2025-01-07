/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{Json, Response};
use chrono::{Duration, Utc};
use core::types::*;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, TokenData, Validation};
use rand::distributions::{Alphanumeric, DistString};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use super::requests::*;

#[derive(Serialize, Deserialize)]
pub struct Cliams {
    pub exp: usize,
    pub iat: usize,
    pub id: Uuid,
}

pub async fn authorize(
    state: State<Arc<ServerState>>,
    mut req: Request,
    next: Next,
) -> Result<Response<Body>, (StatusCode, Json<BaseResponse<String>>)> {
    let auth_header = req.headers_mut().get(axum::http::header::AUTHORIZATION);

    let auth_header = match auth_header {
        Some(header) => header.to_str().map_err(|_| {
            (
                StatusCode::FORBIDDEN,
                Json(BaseResponse {
                    error: true,
                    message: "Authorization header empty".to_string(),
                }),
            )
        })?,
        None => {
            return Err((
                StatusCode::FORBIDDEN,
                Json(BaseResponse {
                    error: true,
                    message: "Authorization header not found".to_string(),
                }),
            ))
        }
    };

    let mut header = auth_header.split_whitespace();

    let (bearer, token) = (header.next(), header.next());

    if bearer != Some("Bearer") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(BaseResponse {
                error: true,
                message: "Invalid Authorization header".to_string(),
            }),
        ));
    }

    let token_data = match decode_jwt(state.clone(), token.unwrap().to_string()).await {
        Ok(data) => data,
        Err(_) => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(BaseResponse {
                    error: true,
                    message: "Unable to decode token".to_string(),
                }),
            ))
        }
    };

    let current_user = match EUser::find_by_id(token_data.claims.id)
        .one(&state.db)
        .await
        .unwrap()
    {
        Some(user) => user,
        None => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(BaseResponse {
                    error: true,
                    message: "User not found".to_string(),
                }),
            ))
        }
    };

    req.extensions_mut().insert(current_user);
    Ok(next.run(req).await)
}

pub fn encode_jwt(state: State<Arc<ServerState>>, id: Uuid) -> Result<String, StatusCode> {
    let now = Utc::now();
    let expire: chrono::TimeDelta = Duration::hours(24);
    let exp: usize = (now + expire).timestamp() as usize;
    let iat: usize = now.timestamp() as usize;

    let claim = Cliams { iat, exp, id };
    let secret = state.cli.jwt_secret.clone();

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
            .filter(CApi::Key.eq(jwt.strip_prefix("GRAD").unwrap()))
            .one(&state.db)
            .await
            .unwrap();

        let api_key = match api_key {
            Some(api_key) => api_key,
            None => return Err(StatusCode::UNAUTHORIZED),
        };

        let mut aapi_key: AApi = api_key.clone().into();

        aapi_key.last_used_at = Set(Utc::now().naive_utc());
        aapi_key.save(&state.db).await.unwrap();

        TokenData {
            claims: Cliams {
                exp: 0,
                iat: api_key.created_at.and_utc().timestamp() as usize,
                id: api_key.owned_by,
            },
            header: Default::default(),
        }
    } else {
        let secret = state.cli.jwt_secret.clone();

        decode(
            &jwt,
            &DecodingKey::from_secret(secret.as_ref()),
            &Validation::default(),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };

    Ok(result)
}

pub async fn update_last_login(state: State<Arc<ServerState>>, user_id: Uuid) {
    let mut auser: AUser = EUser::find_by_id(user_id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap()
        .into();

    auser.last_login_at = Set(Utc::now().naive_utc());
    auser.save(&state.db).await.unwrap();
}

pub fn generate_api_key() -> String {
    Alphanumeric.sample_string(&mut rand::thread_rng(), 64)
}
