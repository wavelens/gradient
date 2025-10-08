/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result, bail};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{Json, Response};
use chrono::{Duration, Utc};
use core::input::load_secret;
use core::types::*;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, TokenData, Validation, decode, encode};
use oauth2::PkceCodeChallenge;
use rand::Rng;
use rand::distr::Alphanumeric;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, Condition, EntityTrait, QueryFilter,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use url::Url;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
pub struct OidcUser {
    pub aud: String,
    pub email: String,
    pub exp: i64,
    pub iat: i64,
    pub iss: String,
    pub name: String,
    pub sub: String,
}

#[derive(Clone, Serialize, Deserialize)]
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
            ));
        }
    };

    let mut header = auth_header.split_whitespace();

    let (bearer, token) = (header.next(), header.next());

    if bearer != Some("Bearer") || token.is_none() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(BaseResponse {
                error: true,
                message: "Invalid Authorization header".to_string(),
            }),
        ));
    }

    let token_str = match token {
        Some(t) => t.to_string(),
        None => {
            return Err((
                StatusCode::FORBIDDEN,
                Json(BaseResponse {
                    error: true,
                    message: "Missing authorization token".to_string(),
                }),
            ));
        }
    };

    let token_data = match decode_jwt(state.clone(), token_str).await {
        Ok(data) => data,
        Err(_) => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(BaseResponse {
                    error: true,
                    message: "Unable to decode token".to_string(),
                }),
            ));
        }
    };

    let current_user = match EUser::find_by_id(token_data.claims.id)
        .one(&state.db)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: "Database error".to_string(),
                }),
            )
        })? {
        Some(user) => user,
        None => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(BaseResponse {
                    error: true,
                    message: "User not found".to_string(),
                }),
            ));
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

pub async fn update_last_login(state: State<Arc<ServerState>>, user: MUser) -> Result<MUser> {
    let mut auser: AUser = user.into();

    auser.last_login_at = Set(Utc::now().naive_utc());
    Ok(auser
        .update(&state.db)
        .await
        .context("Failed to update user last login")?)
}

pub fn generate_api_key() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(64)
        .map(char::from)
        .collect()
}

async fn get_oidc_metadata(discovery_url: &str) -> Result<serde_json::Value> {
    let http_client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .context("Failed to create HTTP client")?;

    let metadata = http_client
        .get(
            if discovery_url.ends_with("/.well-known/openid-configuration") {
                discovery_url.to_string()
            } else {
                format!(
                    "{}/.well-known/openid-configuration",
                    discovery_url.trim_end_matches('/')
                )
            },
        )
        .send()
        .await
        .context("Failed to fetch OIDC metadata")?
        .json::<serde_json::Value>()
        .await
        .context("Failed to parse OIDC metadata")?;

    Ok(metadata)
}

pub async fn oidc_login_create(state: State<Arc<ServerState>>) -> Result<Url> {
    if !state.cli.oidc_enabled {
        bail!("OIDC is not enabled");
    }

    let discovery_url = state
        .cli
        .oidc_discovery_url
        .as_ref()
        .context("OIDC discovery URL not configured")?;

    let metadata = get_oidc_metadata(discovery_url).await?;

    let auth_endpoint = metadata["authorization_endpoint"]
        .as_str()
        .context("No authorization_endpoint in OIDC metadata")?;

    let client_id = state
        .cli
        .oidc_client_id
        .as_ref()
        .context("OIDC client ID not configured")?;

    let redirect_uri = format!("{}/api/v1/auth/oidc/callback", state.cli.serve_url);

    let (pkce_challenge, _pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let state_param = uuid::Uuid::new_v4().to_string();

    let mut params = vec![
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", &redirect_uri),
        ("code_challenge", pkce_challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", &state_param),
    ];

    if let Some(scopes) = &state.cli.oidc_scopes {
        params.push(("scope", scopes));
    } else {
        params.push(("scope", "openid email profile"));
    }

    let auth_url = Url::parse_with_params(auth_endpoint, &params)
        .context("Failed to build authorization URL")?;

    Ok(auth_url)
}

pub async fn oidc_login_verify(
    state: State<Arc<ServerState>>,
    authorization_code: String,
) -> Result<MUser> {
    if !state.cli.oidc_enabled {
        bail!("OIDC is not enabled");
    }

    let discovery_url = state
        .cli
        .oidc_discovery_url
        .as_ref()
        .context("OIDC discovery URL not configured")?;

    let metadata = get_oidc_metadata(discovery_url).await?;

    let token_endpoint = metadata["token_endpoint"]
        .as_str()
        .context("No token_endpoint in OIDC metadata")?;

    let userinfo_endpoint = metadata["userinfo_endpoint"]
        .as_str()
        .context("No userinfo_endpoint in OIDC metadata")?;

    let client_id = state
        .cli
        .oidc_client_id
        .as_ref()
        .context("OIDC client ID not configured")?;
    let client_secret_file = state
        .cli
        .oidc_client_secret_file
        .as_ref()
        .context("OIDC client secret file not configured")?;

    let http_client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .context("Failed to create HTTP client")?;

    let redirect_uri = format!("{}/api/v1/auth/oidc/callback", state.cli.serve_url);

    // Exchange authorization code for tokens
    let token_response = http_client
        .post(token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", &authorization_code),
            ("redirect_uri", &redirect_uri),
            ("client_id", client_id),
            ("client_secret", &load_secret(client_secret_file)),
        ])
        .send()
        .await
        .context("Token exchange request failed")?;

    let token_data: serde_json::Value = token_response
        .json()
        .await
        .context("Failed to parse token response")?;

    let access_token = token_data["access_token"]
        .as_str()
        .context("No access token in response")?;

    // Get user info using access token
    let userinfo_response = http_client
        .get(userinfo_endpoint)
        .bearer_auth(access_token)
        .send()
        .await
        .context("Failed to fetch user info")?;

    let user_data: serde_json::Value = userinfo_response
        .json()
        .await
        .context("Failed to parse user info")?;

    let user_info = OidcUser {
        aud: user_data["aud"].as_str().unwrap_or_default().to_string(),
        email: user_data["email"].as_str().unwrap_or_default().to_string(),
        exp: user_data["exp"].as_i64().unwrap_or(0),
        iat: user_data["iat"].as_i64().unwrap_or(0),
        iss: user_data["iss"].as_str().unwrap_or_default().to_string(),
        name: user_data["name"].as_str().unwrap_or_default().to_string(),
        sub: user_data["sub"].as_str().unwrap_or_default().to_string(),
    };

    create_or_update_user(state, user_info).await
}

async fn create_or_update_user(
    state: State<Arc<ServerState>>,
    user_info: OidcUser,
) -> Result<MUser> {
    let user: Result<MUser> = match EUser::find()
        .filter(
            Condition::any()
                .add(CUser::Email.eq(&user_info.email))
                .add(CUser::Username.eq(&user_info.sub)),
        )
        .one(&state.db)
        .await
        .context("Database error while finding user")?
    {
        Some(mut user) => {
            if user.password.is_some() {
                bail!("User already exists with password authentication");
            }

            let mut updated = false;

            if user.email != user_info.email {
                let mut auser: AUser = user.into();
                auser.email = Set(user_info.email.clone());
                user = auser
                    .update(&state.db)
                    .await
                    .context("Failed to update user email")?;
                updated = true;
            }

            if user.username != user_info.sub {
                let mut auser: AUser = user.into();
                auser.username = Set(user_info.sub.clone());
                user = auser
                    .update(&state.db)
                    .await
                    .context("Failed to update username")?;
                updated = true;
            }

            if user.name != user_info.name {
                let mut auser: AUser = user.into();
                auser.name = Set(user_info.name.clone());
                user = auser
                    .update(&state.db)
                    .await
                    .context("Failed to update user name")?;
                updated = true;
            }

            if updated {
                user = update_last_login(state.clone(), user)
                    .await
                    .context("Failed to update user")?;
            }

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
                email_verified: Set(true), // OIDC users are considered verified
                email_verification_token: Set(None),
                email_verification_token_expires: Set(None),
                managed: Set(false),
            };

            let user = new_user
                .insert(&state.db)
                .await
                .context("Failed to create user")?;

            Ok(user)
        }
    };

    user
}
