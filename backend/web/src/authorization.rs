/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
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
use core::input::load_secret;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, TokenData, Validation};
use oauth2::basic::BasicClient;
use oauth2::reqwest;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, RedirectUrl, Scope,
    TokenResponse, TokenUrl,
};
use rand::distr::{Alphanumeric, SampleString};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, Condition, EntityTrait, QueryFilter,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use url::Url;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
pub struct OAuthUser {
    pub aud: String,
    pub email: String,
    pub exp: i64,
    pub iat: i64,
    pub iss: String,
    pub name: String,
    pub sub: String,
}

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

pub async fn update_last_login(
    state: State<Arc<ServerState>>,
    user: MUser,
) -> Result<MUser, String> {
    let mut auser: AUser = user.into();

    auser.last_login_at = Set(Utc::now().naive_utc());
    Ok(auser.update(&state.db).await.unwrap())
}

pub fn generate_api_key() -> String {
    Alphanumeric.sample_string(&mut rand::rng(), 64)
}

pub fn oauth_login_create(state: State<Arc<ServerState>>) -> Result<Url, String> {
    if !state.cli.oauth_enabled {
        return Err("OAuth is not enabled".to_string());
    }

    // TODO: Cleaner way to get OAuth client
    let client = if let (
        Some(oauth_client_id),
        Some(oauth_client_secret_file),
        Some(oauth_auth_url),
        Some(oauth_token_url),
    ) = (
        state.cli.oauth_client_id.clone(),
        state.cli.oauth_client_secret_file.clone(),
        state.cli.oauth_auth_url.clone(),
        state.cli.oauth_token_url.clone(),
    ) {
        let client = BasicClient::new(ClientId::new(oauth_client_id))
            .set_client_secret(ClientSecret::new(load_secret(&oauth_client_secret_file)))
            .set_auth_uri(AuthUrl::new(oauth_auth_url).unwrap())
            .set_token_uri(TokenUrl::new(oauth_token_url).unwrap())
            .set_redirect_uri(
                RedirectUrl::new(format!(
                    "{}/api/v1/auth/oauth/authorize",
                    state.cli.serve_url.clone()
                ))
                .unwrap(),
            );

        Ok(client)
    } else {
        Err("OAuth configuration is not set".to_string())
    }
    .unwrap();

    // TODO: Implement PKCE
    // let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

    let (authorize_url, _csrf_state) = if let Some(scopes) = state.cli.oauth_scopes.clone() {
        client
            .authorize_url(CsrfToken::new_random)
            .add_scopes(
                scopes
                    .split_whitespace()
                    .map(|v: &str| Scope::new(v.to_string())),
            )
            // .set_pkce_challenge(pkce_challenge)
            .url()
    } else {
        client
            .authorize_url(CsrfToken::new_random)
            // .set_pkce_challenge(pkce_challenge)
            .url()
    };

    Ok(authorize_url)
}

pub async fn oauth_login_verify(
    state: State<Arc<ServerState>>,
    access_token: String,
) -> Result<MUser, String> {
    if !state.cli.oauth_enabled {
        return Err("OAuth is not enabled".to_string());
    }

    let client = if let (
        Some(oauth_client_id),
        Some(oauth_client_secret_file),
        Some(oauth_auth_url),
        Some(oauth_token_url),
    ) = (
        state.cli.oauth_client_id.clone(),
        state.cli.oauth_client_secret_file.clone(),
        state.cli.oauth_auth_url.clone(),
        state.cli.oauth_token_url.clone(),
    ) {
        let client = BasicClient::new(ClientId::new(oauth_client_id))
            .set_client_secret(ClientSecret::new(load_secret(&oauth_client_secret_file)))
            .set_auth_uri(AuthUrl::new(oauth_auth_url).unwrap())
            .set_token_uri(TokenUrl::new(oauth_token_url).unwrap())
            .set_redirect_uri(
                RedirectUrl::new(format!(
                    "{}/api/v1/auth/oauth/authorize",
                    state.cli.serve_url.clone()
                ))
                .unwrap(),
            );

        Ok(client)
    } else {
        Err("OAuth configuration is not set".to_string())
    }
    .unwrap();

    let http_client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("Client should build");

    // TODO: Implement PKCE and verify CSRF

    let token_result = client
        .exchange_code(AuthorizationCode::new(access_token))
        // .set_pkce_verifier(client_data.pkce_verifier)
        .request_async(&http_client)
        .await
        .unwrap();

    let token = token_result.access_token().secret();

    let user_info = if let Some(oauth_api_url) = state.cli.oauth_api_url.clone() {
        let user_info_json = http_client
            .get(oauth_api_url)
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        let user_info: OAuthUser = serde_json::from_str(&user_info_json).unwrap();

        Ok(user_info)
    } else {
        Err("OAuth configuration is not set".to_string())
    }
    .unwrap();

    let user: Result<MUser, String> = match EUser::find()
        .filter(
            Condition::any()
                .add(CUser::Email.eq(&user_info.email))
                .add(CUser::Username.eq(&user_info.sub)),
        )
        .one(&state.db)
        .await
        .unwrap()
    {
        Some(mut user) => {
            if user.password.is_some() {
                return Err("User already exists".to_string());
            }

            if user.email != user_info.email {
                let mut auser: AUser = user.into();

                auser.email = Set(user_info.email.clone());
                user = auser.update(&state.db).await.unwrap();
            }

            if user.username != user_info.sub {
                let mut auser: AUser = user.into();

                auser.username = Set(user_info.sub.clone());
                user = auser.update(&state.db).await.unwrap();
            }

            if user.name != user_info.name {
                let mut auser: AUser = user.into();

                auser.name = Set(user_info.name.clone());
                user = auser.update(&state.db).await.unwrap();
            }

            user = update_last_login(state.clone(), user)
                .await
                .map_err(|_| "Failed to update user".to_string())
                .unwrap();

            Ok(user)
        }
        None => {
            let new_user = AUser {
                id: Set(Uuid::new_v4()),
                username: Set(user_info.sub.clone()),
                name: Set(user_info.name.clone()),
                email: Set(user_info.email.clone()),
                password: Set(None),
                last_login_at: Set(Utc::now().naive_utc()),
                created_at: Set(Utc::now().naive_utc()),
            };

            let user = new_user
                .insert(&state.db)
                .await
                .map_err(|_| "Failed to create user".to_string())
                .unwrap();

            Ok(user)
        }
    };

    Ok(user.unwrap())
}
