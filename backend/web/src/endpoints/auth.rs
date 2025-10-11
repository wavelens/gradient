/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{encode_jwt, oidc_login_create, oidc_login_verify, update_last_login};
use crate::error::{WebError, WebResult};
use axum::Json;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::Utc;
use core::consts::*;
use core::email::{EmailService, generate_verification_token};
use core::input::{validate_display_name, validate_password, validate_username};
use core::types::*;
use email_address::EmailAddress;
use password_auth::{generate_hash, verify_password};
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeLoginRequest {
    pub loginname: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeUserRequest {
    pub username: String,
    pub name: String,
    pub email: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CheckUsernameRequest {
    pub username: String,
}

pub async fn post_basic_register(
    state: State<Arc<ServerState>>,
    Json(body): Json<MakeUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if state.cli.oidc_required || state.cli.disable_registration {
        return Err(WebError::registration_disabled());
    }

    if let Err(e) = validate_username(&body.username) {
        return Err(WebError::invalid_username(e.to_string()));
    }

    if let Err(e) = validate_display_name(&body.name) {
        return Err(WebError::BadRequest(format!("Invalid name: {}", e)));
    }

    if !EmailAddress::is_valid(body.email.clone().as_str()) {
        return Err(WebError::invalid_email());
    }

    if let Err(e) = validate_password(&body.password) {
        return Err(WebError::invalid_password(e.to_string()));
    }

    let existing_user = EUser::find()
        .filter(
            Condition::any()
                .add(CUser::Username.eq(body.username.clone()))
                .add(CUser::Email.eq(body.email.clone())),
        )
        .one(&state.db)
        .await?;

    if existing_user.is_some() {
        return Err(WebError::already_exists("User"));
    }

    let email_service = EmailService::new(&state.cli).await.map_err(|e| {
        WebError::InternalServerError(format!("Failed to initialize email service: {}", e))
    })?;

    let (email_verified, verification_token, verification_expires) =
        if state.cli.email_enabled && state.cli.email_require_verification {
            let token = generate_verification_token();
            let expires = Utc::now().naive_utc() + chrono::Duration::hours(24);
            (false, Some(token), Some(expires))
        } else {
            (true, None, None)
        };

    let user = AUser {
        id: Set(Uuid::new_v4()),
        username: Set(body.username.clone()),
        name: Set(body.name.clone()),
        email: Set(body.email.clone()),
        password: Set(Some(generate_hash(body.password.clone()))),
        last_login_at: Set(*NULL_TIME),
        created_at: Set(Utc::now().naive_utc()),
        email_verified: Set(email_verified),
        email_verification_token: Set(verification_token.clone()),
        email_verification_token_expires: Set(verification_expires),
        managed: Set(false),
    };

    let user = user.insert(&state.db).await?;

    if state.cli.email_enabled
        && state.cli.email_require_verification
        && let Some(ref token) = verification_token
        && let Err(e) = email_service
            .send_verification_email(&body.email, &body.name, token, &state.cli.serve_url)
            .await
    {
        tracing::warn!("Failed to send verification email: {}", e);
    }

    let message = if state.cli.email_enabled && state.cli.email_require_verification {
        format!(
            "User {} created. Please check your email to verify your account.",
            user.id
        )
    } else {
        format!("User {} created successfully.", user.id)
    };

    let res = BaseResponse {
        error: false,
        message,
    };

    Ok(Json(res))
}

pub async fn post_basic_login(
    state: State<Arc<ServerState>>,
    Json(body): Json<MakeLoginRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if state.cli.oidc_required {
        return Err(WebError::oauth_required());
    }

    let user = EUser::find()
        .filter(
            Condition::any()
                .add(CUser::Username.eq(body.loginname.clone()))
                .add(CUser::Email.eq(body.loginname.clone())),
        )
        .one(&state.db)
        .await?
        .ok_or_else(WebError::invalid_credentials)?;

    let user_password = user
        .password
        .clone()
        .ok_or_else(WebError::oauth_required)?;

    verify_password(body.password, &user_password).map_err(|_| WebError::invalid_credentials())?;

    if state.cli.email_enabled && state.cli.email_require_verification && !user.email_verified {
        return Err(WebError::BadRequest("Email not verified. Please check your email and verify your account before logging in.".to_string()));
    }

    let token =
        encode_jwt(state.clone(), user.id).map_err(|_| WebError::failed_to_generate_token())?;

    update_last_login(state, user)
        .await
        .map_err(|_| WebError::failed_to_update_user())?;

    let res = BaseResponse {
        error: false,
        message: token,
    };

    Ok(Json(res))
}

pub async fn get_oauth_authorize(
    state: State<Arc<ServerState>>,
    Query(query): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let code = query
        .get("code")
        .ok_or_else(WebError::invalid_oauth_code)?;

    let user: MUser = oidc_login_verify(state.clone(), code.to_string())
        .await
        .map_err(|e| WebError::InternalServerError(e.to_string()))?;

    let token = encode_jwt(state, user.id).map_err(|_| WebError::failed_to_generate_token())?;

    let res = BaseResponse {
        error: false,
        message: token,
    };

    Ok(Json(res))
}

pub async fn post_oauth_authorize(
    state: State<Arc<ServerState>>,
) -> WebResult<Json<BaseResponse<String>>> {
    if !state.cli.oidc_enabled {
        return Err(WebError::oauth_disabled());
    }

    let authorize_url = oidc_login_create(state)
        .await
        .map_err(|e| WebError::Unauthorized(e.to_string()))?;

    let res = BaseResponse {
        error: false,
        message: authorize_url.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_oidc_login(
    state: State<Arc<ServerState>>,
    Query(_query): Query<HashMap<String, String>>,
) -> WebResult<Response> {
    if !state.cli.oidc_enabled {
        return Err(WebError::oauth_disabled());
    }

    let authorize_url = oidc_login_create(state)
        .await
        .map_err(|e| WebError::Unauthorized(e.to_string()))?;

    match Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", authorize_url.to_string())
        .body(Body::empty())
    {
        Ok(response) => Ok(response),
        Err(e) => {
            tracing::error!("Failed to build HTTP response: {}", e);
            Err(WebError::InternalServerError(
                "Failed to build redirect response".to_string(),
            ))
        }
    }
}

pub async fn get_oidc_callback(
    state: State<Arc<ServerState>>,
    Query(query): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<String>>> {
    let code = query
        .get("code")
        .ok_or_else(WebError::invalid_oauth_code)?;

    let user: MUser = oidc_login_verify(state.clone(), code.to_string())
        .await
        .map_err(|e| WebError::InternalServerError(e.to_string()))?;

    let token = encode_jwt(state, user.id).map_err(|_| WebError::failed_to_generate_token())?;

    let res = BaseResponse {
        error: false,
        message: token,
    };

    Ok(Json(res))
}

pub async fn post_logout(_state: State<Arc<ServerState>>) -> WebResult<Json<BaseResponse<String>>> {
    // TODO: invalidate token if needed
    let res = BaseResponse {
        error: false,
        message: "Logout Successfully".to_string(),
    };

    Ok(Json(res))
}

pub async fn post_check_username(
    state: State<Arc<ServerState>>,
    Json(body): Json<CheckUsernameRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    // First validate the username format
    if let Err(e) = validate_username(&body.username) {
        return Ok(Json(BaseResponse {
            error: true,
            message: e.to_string(),
        }));
    }

    // Check if username already exists
    let existing_user = EUser::find()
        .filter(CUser::Username.eq(body.username.clone()))
        .one(&state.db)
        .await?;

    if existing_user.is_some() {
        return Ok(Json(BaseResponse {
            error: true,
            message: "Username is already taken".to_string(),
        }));
    }

    // Username is available
    Ok(Json(BaseResponse {
        error: false,
        message: "Username is available".to_string(),
    }))
}

#[derive(Serialize, Deserialize, Debug)]
pub struct VerifyEmailRequest {
    pub token: String,
}

pub async fn get_verify_email(
    state: State<Arc<ServerState>>,
    Query(query): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<String>>> {
    if !state.cli.email_enabled || !state.cli.email_require_verification {
        return Err(WebError::BadRequest(
            "Email verification is not enabled".to_string(),
        ));
    }

    let token = query
        .get("token")
        .ok_or_else(|| WebError::BadRequest("Missing verification token".to_string()))?;

    let user = EUser::find()
        .filter(CUser::EmailVerificationToken.eq(token.clone()))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::BadRequest("Invalid verification token".to_string()))?;

    if let Some(expires) = user.email_verification_token_expires
        && Utc::now().naive_utc() > expires
    {
        return Err(WebError::BadRequest(
            "Verification token has expired".to_string(),
        ));
    }

    if user.email_verified {
        return Ok(Json(BaseResponse {
            error: false,
            message: "Email already verified".to_string(),
        }));
    }

    let mut user_active: AUser = user.into();
    user_active.email_verified = Set(true);
    user_active.email_verification_token = Set(None);
    user_active.email_verification_token_expires = Set(None);

    user_active.update(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Email verified successfully".to_string(),
    }))
}

pub async fn post_resend_verification(
    state: State<Arc<ServerState>>,
    Json(body): Json<CheckUsernameRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if !state.cli.email_enabled || !state.cli.email_require_verification {
        return Err(WebError::BadRequest(
            "Email verification is not enabled".to_string(),
        ));
    }

    let user = EUser::find()
        .filter(CUser::Username.eq(body.username.clone()))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("User"))?;

    if user.email_verified {
        return Err(WebError::BadRequest(
            "Email is already verified".to_string(),
        ));
    }

    let email_service = EmailService::new(&state.cli).await.map_err(|e| {
        WebError::InternalServerError(format!("Failed to initialize email service: {}", e))
    })?;

    let verification_token = generate_verification_token();
    let verification_expires = Utc::now().naive_utc() + chrono::Duration::hours(24);

    let mut user_active: AUser = user.clone().into();
    user_active.email_verification_token = Set(Some(verification_token.clone()));
    user_active.email_verification_token_expires = Set(Some(verification_expires));

    user_active.update(&state.db).await?;

    if let Err(e) = email_service
        .send_verification_email(
            &user.email,
            &user.name,
            &verification_token,
            &state.cli.serve_url,
        )
        .await
    {
        return Err(WebError::InternalServerError(format!(
            "Failed to send verification email: {}",
            e
        )));
    }

    Ok(Json(BaseResponse {
        error: false,
        message: "Verification email sent successfully".to_string(),
    }))
}
