/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::{
    create_session_and_token, oidc_login_create, oidc_login_verify, update_last_login,
};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use axum::Json;
use axum::body::Body;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use std::net::SocketAddr;
use axum::response::{IntoResponse, Response};

use email_address::EmailAddress;
use gradient_core::storage::generate_verification_token;
use gradient_core::types::consts::*;
use gradient_core::types::input::{validate_display_name, validate_password, validate_username};
use gradient_core::types::*;
use password_auth::{generate_hash, verify_password};
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, QueryFilter,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeLoginRequest {
    pub loginname: String,
    pub password: String,
    #[serde(default)]
    pub remember_me: bool,
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
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<MakeUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if !state.config.registration.enable_registration
        || state.config.oidc.as_ref().is_some_and(|o| o.required)
    {
        return Err(WebError::registration_disabled());
    }

    if let Err(e) = validate_username(&body.username) {
        return Err(WebError::invalid_username(e.to_string()));
    }

    if let Err(e) = validate_display_name(&body.name) {
        return Err(WebError::bad_request(format!("Invalid name: {}", e)));
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
        .one(&state.web_db)
        .await?;

    if existing_user.is_some() {
        return Err(WebError::already_exists("User"));
    }

    let (email_verified, verification_token, verification_expires) = if state
        .config
        .email
        .as_ref()
        .is_some_and(|e| e.require_verification)
    {
        let token = generate_verification_token();
        let expires = gradient_core::types::now() + chrono::Duration::hours(24);
        (false, Some(token), Some(expires))
    } else {
        (true, None, None)
    };

    let user = AUser {
        id: Set(UserId::now_v7()),
        username: Set(body.username.clone()),
        name: Set(body.name.clone()),
        email: Set(body.email.clone()),
        password: Set(Some(generate_hash(body.password.clone()))),
        last_login_at: Set(*NULL_TIME),
        created_at: Set(gradient_core::types::now()),
        email_verified: Set(email_verified),
        email_verification_token: Set(verification_token.clone()),
        email_verification_token_expires: Set(verification_expires),
        managed: Set(false),
        superuser: Set(false),
        oidc_issuer: Set(None),
        oidc_subject: Set(None),
    };

    let user = user
        .insert(&state.web_db)
        .await
        .map_err(|e| WebError::from_db_err(e, "User"))?;

    let info = RequestInfo::from_request(&headers, addr.ip(), &state.config.network.trusted_proxies);
    audit_record(
        &state.web_db,
        Some(user.id),
        events::REGISTER,
        &info,
        Some(serde_json::json!({ "username": user.username })),
    )
    .await;

    if state
        .config
        .email
        .as_ref()
        .is_some_and(|e| e.require_verification)
        && let Some(ref token) = verification_token
        && let Err(e) = state
            .email
            .send_verification_email(
                &body.email,
                &body.name,
                token,
                &state.config.server.serve_url,
            )
            .await
    {
        tracing::warn!(error = %e, "Failed to send verification email");
    }

    let message = if state
        .config
        .email
        .as_ref()
        .is_some_and(|e| e.require_verification)
    {
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
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<MakeLoginRequest>,
) -> WebResult<Response> {
    if state.config.oidc.as_ref().is_some_and(|o| o.required) {
        return Err(WebError::oauth_required());
    }

    let info = RequestInfo::from_request(&headers, addr.ip(), &state.config.network.trusted_proxies);

    let user = match EUser::find()
        .filter(
            Condition::any()
                .add(CUser::Username.eq(body.loginname.clone()))
                .add(CUser::Email.eq(body.loginname.clone())),
        )
        .one(&state.web_db)
        .await?
    {
        Some(u) => u,
        None => {
            audit_record(
                &state.web_db,
                None,
                events::LOGIN_FAILURE,
                &info,
                Some(serde_json::json!({ "loginname": body.loginname })),
            )
            .await;
            return Err(WebError::invalid_credentials());
        }
    };

    let user_password = user.password.clone().ok_or_else(WebError::oauth_required)?;

    if verify_password(body.password, &user_password).is_err() {
        audit_record(
            &state.web_db,
            Some(user.id),
            events::LOGIN_FAILURE,
            &info,
            None,
        )
        .await;
        return Err(WebError::invalid_credentials());
    }

    if state
        .config
        .email
        .as_ref()
        .is_some_and(|e| e.require_verification)
        && !user.email_verified
    {
        return Err(WebError::bad_request(
            "Email not verified. Please check your email and verify your account before logging in.",
        ));
    }

    let use_tls = state.config.server.use_tls;
    let (_session_id, token) = create_session_and_token(
        state.clone(),
        user.id,
        body.remember_me,
        info.user_agent.clone(),
        info.ip.clone(),
    )
    .await
    .map_err(|_| WebError::failed_to_generate_token())?;

    let user_id = user.id;
    update_last_login(state.clone(), user)
        .await
        .map_err(|_| WebError::failed_to_update_user())?;

    audit_record(
        &state.web_db,
        Some(user_id),
        events::LOGIN_SUCCESS,
        &info,
        None,
    )
    .await;

    let cookie = jwt_cookie(&token, body.remember_me, use_tls);
    let res = BaseResponse {
        error: false,
        message: token,
    };
    let mut response = Json(res).into_response();
    response.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| WebError::internal("Bad cookie"))?,
    );
    Ok(response)
}

const OIDC_CSRF_COOKIE: &str = "oidc_csrf";

fn oidc_csrf_set_cookie(value: &str, use_tls: bool) -> String {
    let secure = if use_tls { "; Secure" } else { "" };
    format!(
        "{}={}; HttpOnly{}; SameSite=Lax; Path=/; Max-Age=600",
        OIDC_CSRF_COOKIE, value, secure
    )
}

fn oidc_csrf_clear_cookie(use_tls: bool) -> String {
    let secure = if use_tls { "; Secure" } else { "" };
    format!(
        "{}=; HttpOnly{}; SameSite=Lax; Path=/; Max-Age=0",
        OIDC_CSRF_COOKIE, secure
    )
}

fn read_cookie(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    let header = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    header
        .split(';')
        .map(str::trim)
        .find_map(|p| p.strip_prefix(&format!("{}=", name)).map(str::to_owned))
}

/// Logs the full anyhow chain at warn level and converts the error into a
/// generic 401 — the upstream IdP / transport detail stays in the operator's
/// log instead of being echoed into the response body where the client (or an
/// attacker probing the endpoint) can read it.
fn oidc_failure(stage: &'static str) -> impl FnOnce(anyhow::Error) -> WebError {
    move |e| {
        tracing::warn!(stage, error = format!("{:#}", e), "OIDC flow failed");
        WebError::unauthorized("OIDC login failed")
    }
}

pub async fn get_oauth_authorize(
    state: State<Arc<ServerState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> WebResult<Response> {
    if state.config.oidc.is_none() {
        return Err(WebError::oauth_disabled());
    }

    let code = query.get("code").ok_or_else(WebError::invalid_oauth_code)?;
    let state_query = query
        .get("state")
        .ok_or_else(|| WebError::unauthorized("Missing OIDC state"))?;
    let csrf = read_cookie(&headers, OIDC_CSRF_COOKIE)
        .ok_or_else(|| WebError::unauthorized("Missing OIDC CSRF cookie"))?;

    let use_tls = state.config.server.use_tls;
    let user: MUser = oidc_login_verify(
        state.clone(),
        code.to_string(),
        state_query.to_string(),
        csrf,
    )
    .await
    .map_err(oidc_failure("oauth_authorize_callback"))?;

    let info = RequestInfo::from_request(&headers, addr.ip(), &state.config.network.trusted_proxies);
    let (_session_id, token) = create_session_and_token(
        state.clone(),
        user.id,
        false,
        info.user_agent.clone(),
        info.ip.clone(),
    )
    .await
    .map_err(|_| WebError::failed_to_generate_token())?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::LOGIN_SUCCESS,
        &info,
        Some(serde_json::json!({ "method": "oidc" })),
    )
    .await;

    let res = BaseResponse {
        error: false,
        message: token,
    };

    let mut response = Json(res).into_response();
    response.headers_mut().append(
        axum::http::header::SET_COOKIE,
        HeaderValue::from_str(&oidc_csrf_clear_cookie(use_tls))
            .map_err(|_| WebError::internal("Bad cookie"))?,
    );
    Ok(response)
}

pub async fn post_oauth_authorize(state: State<Arc<ServerState>>) -> WebResult<Response> {
    if state.config.oidc.is_none() {
        return Err(WebError::oauth_disabled());
    }

    let use_tls = state.config.server.use_tls;
    let req = oidc_login_create(state)
        .await
        .map_err(oidc_failure("oauth_authorize_start"))?;

    let res = BaseResponse {
        error: false,
        message: req.auth_url.to_string(),
    };
    let mut response = Json(res).into_response();
    response.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        HeaderValue::from_str(&oidc_csrf_set_cookie(&req.cookie_value, use_tls))
            .map_err(|_| WebError::internal("Bad cookie"))?,
    );
    Ok(response)
}

pub async fn get_oidc_login(
    state: State<Arc<ServerState>>,
    Query(_query): Query<HashMap<String, String>>,
) -> WebResult<Response> {
    if state.config.oidc.is_none() {
        return Err(WebError::oauth_disabled());
    }

    let use_tls = state.config.server.use_tls;
    let req = oidc_login_create(state)
        .await
        .map_err(oidc_failure("oidc_login_start"))?;

    Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", req.auth_url.to_string())
        .header(
            "Set-Cookie",
            oidc_csrf_set_cookie(&req.cookie_value, use_tls),
        )
        .body(Body::empty())
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to build HTTP response");
            WebError::internal("Failed to build redirect response")
        })
}

pub async fn get_oidc_callback(
    state: State<Arc<ServerState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> WebResult<Response> {
    let code = query.get("code").ok_or_else(WebError::invalid_oauth_code)?;
    let state_query = query
        .get("state")
        .ok_or_else(|| WebError::unauthorized("Missing OIDC state"))?;
    let csrf = read_cookie(&headers, OIDC_CSRF_COOKIE)
        .ok_or_else(|| WebError::unauthorized("Missing OIDC CSRF cookie"))?;

    let use_tls = state.config.server.use_tls;
    let user: MUser = oidc_login_verify(
        state.clone(),
        code.to_string(),
        state_query.to_string(),
        csrf,
    )
    .await
    .map_err(oidc_failure("oidc_callback"))?;

    let info = RequestInfo::from_request(&headers, addr.ip(), &state.config.network.trusted_proxies);
    let (_session_id, token) = create_session_and_token(
        state.clone(),
        user.id,
        false,
        info.user_agent.clone(),
        info.ip.clone(),
    )
    .await
    .map_err(|_| WebError::failed_to_generate_token())?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::LOGIN_SUCCESS,
        &info,
        Some(serde_json::json!({ "method": "oidc" })),
    )
    .await;

    let session_cookie = jwt_cookie(&token, false, use_tls);
    let clear_csrf = oidc_csrf_clear_cookie(use_tls);

    Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", "/account/oidc-callback")
        .header("Set-Cookie", &session_cookie)
        .header("Set-Cookie", &clear_csrf)
        .body(Body::empty())
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to build HTTP response");
            WebError::internal("Failed to build redirect response")
        })
}

pub async fn post_logout(
    state: State<Arc<ServerState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> WebResult<Response> {
    let secure = if state.config.server.use_tls {
        "; Secure"
    } else {
        ""
    };
    let clear_cookie = format!(
        "jwt_token=; HttpOnly{}; SameSite=Strict; Path=/; Max-Age=0",
        secure
    );

    if let Some(token) = crate::authorization::extract_bearer_or_cookie(&headers)
        && !token.starts_with("GRAD")
        && let Ok(data) = jsonwebtoken::decode::<crate::authorization::Cliams>(
            &token,
            &jsonwebtoken::DecodingKey::from_secret(state.jwt_secret.expose().as_bytes()),
            &jsonwebtoken::Validation::default(),
        )
    {
        let session_id = data.claims.jti;
        let user_id = data.claims.id;
        if let Ok(Some(session)) = ESession::find_by_id(session_id).one(&state.web_db).await {
            let mut active: ASession = session.into_active_model();
            active.revoked_at = Set(Some(gradient_core::types::now()));
            let _ = active.update(&state.web_db).await;
        }

        let info = RequestInfo::from_request(&headers, addr.ip(), &state.config.network.trusted_proxies);
        audit_record(
            &state.web_db,
            Some(user_id),
            events::LOGOUT,
            &info,
            Some(serde_json::json!({ "session_id": session_id.to_string() })),
        )
        .await;
    }

    let res = BaseResponse {
        error: false,
        message: "Logout Successfully".to_string(),
    };
    let mut response = Json(res).into_response();
    response.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        HeaderValue::from_str(&clear_cookie).map_err(|_| WebError::internal("Bad cookie"))?,
    );
    Ok(response)
}

fn jwt_cookie(token: &str, remember_me: bool, use_tls: bool) -> String {
    let secure = if use_tls { "; Secure" } else { "" };
    let base = format!(
        "jwt_token={}; HttpOnly{}; SameSite=Strict; Path=/",
        token, secure
    );
    if remember_me {
        format!("{}; Max-Age=2592000", base) // 30 days
    } else {
        base
    }
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
        .one(&state.web_db)
        .await?;

    if existing_user.is_some() {
        return Ok(Json(BaseResponse {
            error: true,
            message: "Username is already taken".to_string(),
        }));
    }

    // Username is available
    Ok(ok_json("Username is available".to_string()))
}

#[derive(Serialize, Deserialize, Debug)]
pub struct VerifyEmailRequest {
    pub token: String,
}

pub async fn get_verify_email(
    state: State<Arc<ServerState>>,
    Query(query): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<String>>> {
    if !state
        .config
        .email
        .as_ref()
        .is_some_and(|e| e.require_verification)
    {
        return Err(WebError::bad_request(
            "Email verification is not enabled".to_string(),
        ));
    }

    let token = query
        .get("token")
        .ok_or_else(|| WebError::bad_request("Missing verification token"))?;

    let user = EUser::find()
        .filter(CUser::EmailVerificationToken.eq(token.clone()))
        .one(&state.web_db)
        .await?
        .ok_or_else(|| WebError::bad_request("Invalid verification token"))?;

    if let Some(expires) = user.email_verification_token_expires
        && gradient_core::types::now() > expires
    {
        return Err(WebError::bad_request(
            "Verification token has expired".to_string(),
        ));
    }

    if user.email_verified {
        return Ok(ok_json("Email already verified".to_string()));
    }

    let mut user_active: AUser = user.into();
    user_active.email_verified = Set(true);
    user_active.email_verification_token = Set(None);
    user_active.email_verification_token_expires = Set(None);

    user_active.update(&state.web_db).await?;

    Ok(ok_json("Email verified successfully".to_string()))
}

pub async fn post_resend_verification(
    state: State<Arc<ServerState>>,
    Json(body): Json<CheckUsernameRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    use sea_orm::TransactionTrait;

    if !state
        .config
        .email
        .as_ref()
        .is_some_and(|e| e.require_verification)
    {
        return Err(WebError::bad_request(
            "Email verification is not enabled".to_string(),
        ));
    }

    let user = EUser::find()
        .filter(CUser::Username.eq(body.username.clone()))
        .one(&state.web_db)
        .await?
        .or_not_found("User")?;

    if user.email_verified {
        return Err(WebError::bad_request(
            "Email is already verified".to_string(),
        ));
    }

    let verification_token = generate_verification_token();
    let verification_expires = gradient_core::types::now() + chrono::Duration::hours(24);

    let tx = state.web_db.inner().begin().await?;

    let mut user_active: AUser = user.clone().into();
    user_active.email_verification_token = Set(Some(verification_token.clone()));
    user_active.email_verification_token_expires = Set(Some(verification_expires));
    user_active.update(&tx).await?;

    state
        .email
        .send_verification_email(
            &user.email,
            &user.name,
            &verification_token,
            &state.config.server.serve_url,
        )
        .await
        .map_err(|e| WebError::internal(format!("Failed to send verification email: {}", e)))?;

    tx.commit().await?;

    Ok(ok_json("Verification email sent successfully".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── jwt_cookie ────────────────────────────────────────────────────────────

    #[test]
    fn jwt_cookie_no_tls_no_remember() {
        let cookie = jwt_cookie("mytoken", false, false);
        assert!(cookie.contains("jwt_token=mytoken"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Path=/"));
        assert!(!cookie.contains("Secure"));
        assert!(!cookie.contains("Max-Age"));
    }

    #[test]
    fn jwt_cookie_tls_adds_secure() {
        let cookie = jwt_cookie("mytoken", false, true);
        assert!(cookie.contains("; Secure"));
        assert!(!cookie.contains("Max-Age"));
    }

    #[test]
    fn jwt_cookie_remember_adds_max_age() {
        let cookie = jwt_cookie("mytoken", true, false);
        assert!(cookie.contains("Max-Age=2592000"));
        assert!(!cookie.contains("Secure"));
    }

    #[test]
    fn jwt_cookie_both_flags() {
        let cookie = jwt_cookie("mytoken", true, true);
        assert!(cookie.contains("; Secure"));
        assert!(cookie.contains("Max-Age=2592000"));
    }

    #[test]
    fn jwt_cookie_contains_token() {
        let cookie = jwt_cookie("tok123", false, false);
        assert!(cookie.starts_with("jwt_token=tok123"));
    }
}
