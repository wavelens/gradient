/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::audit::Audit;
use crate::authorization::{
    cookie_value, create_session_and_token, oidc_login_create, oidc_login_verify, session_claims,
    update_last_login,
};
use crate::error::{ErrorCode, WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::json_body::JsonBody;
use crate::names::UserDisplayName;
use crate::refusal::{Refusal, RefusalStatus, SharedRefusal};
use crate::resource::Resource;
use anyhow::Context;
use axum::Extension;
use axum::Json;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use gradient_db::WebDb;
use gradient_db::lookup::by_id;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;

use chrono::Duration;
use email_address::EmailAddress;
use gradient_core::ServerState;
use gradient_notify::generate_verification_token;
use gradient_types::consts::*;
use gradient_types::input::{validate_password, validate_username};
use gradient_types::*;
use password_auth::{generate_hash, verify_password};
use rand::distr::{Alphanumeric, SampleString};
use rand::seq::IndexedRandom;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
enum AuthRefusal {
    #[error(
        "Email not verified. Please check your email and verify your account before logging in."
    )]
    EmailNotVerified,
    #[error("OIDC login failed")]
    OidcLoginFailed,
    #[error("Missing OIDC state")]
    MissingOidcState,
    #[error("Missing OIDC CSRF cookie")]
    MissingOidcCsrfCookie,
    #[error("Email verification is not enabled")]
    EmailVerificationDisabled,
    #[error("Missing verification token")]
    MissingVerificationToken,
    #[error("Invalid verification token")]
    InvalidVerificationToken,
    #[error("Verification token has expired")]
    ExpiredVerificationToken,
    #[error("Email is already verified")]
    EmailAlreadyVerified,
    #[error("Authorization denied")]
    DeviceAuthorizationDenied,
    #[error("Authorization expired")]
    DeviceAuthorizationExpired,
    #[error("Authorization pending")]
    DeviceAuthorizationPending,
    #[error("Authorization already denied")]
    DeviceAuthorizationAlreadyDenied,
    #[error("Authorization already completed")]
    DeviceAuthorizationAlreadyCompleted,
}

impl Refusal for AuthRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::EmailNotVerified
            | Self::EmailVerificationDisabled
            | Self::MissingVerificationToken
            | Self::InvalidVerificationToken
            | Self::ExpiredVerificationToken
            | Self::EmailAlreadyVerified
            | Self::DeviceAuthorizationDenied
            | Self::DeviceAuthorizationExpired
            | Self::DeviceAuthorizationPending
            | Self::DeviceAuthorizationAlreadyDenied
            | Self::DeviceAuthorizationAlreadyCompleted => RefusalStatus::BadRequest,
            Self::OidcLoginFailed | Self::MissingOidcState | Self::MissingOidcCsrfCookie => {
                RefusalStatus::Unauthorized
            }
        }
    }

    fn code(&self) -> ErrorCode {
        match self {
            Self::DeviceAuthorizationDenied
            | Self::DeviceAuthorizationAlreadyDenied
            | Self::DeviceAuthorizationAlreadyCompleted => ErrorCode::CLI_AUTH_DENIED,
            Self::DeviceAuthorizationExpired => ErrorCode::CLI_AUTH_EXPIRED,
            Self::DeviceAuthorizationPending => ErrorCode::CLI_AUTH_PENDING,
            _ => self.status().default_code(),
        }
    }
}

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
    pub name: UserDisplayName,
    pub email: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CheckUsernameRequest {
    pub username: String,
}

pub async fn post_basic_register(
    state: State<Arc<ServerState>>,
    audit: Audit,
    JsonBody(body): JsonBody<MakeUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if !state.config.registration.enable || state.config.oidc.as_ref().is_some_and(|o| o.required) {
        return Err(WebError::registration_disabled());
    }

    if let Err(e) = validate_username(&body.username) {
        return Err(WebError::invalid_username(e.to_string()));
    }

    if !EmailAddress::is_valid(body.email.clone().as_str()) {
        return Err(WebError::invalid_email());
    }

    if let Err(e) = validate_password(&body.password) {
        return Err(WebError::invalid_password(e.to_string()));
    }

    let existing_user =
        gradient_db::users::by_username_or_email(&state.web_db, &body.username, &body.email)
            .await?;

    if existing_user.is_some() {
        return Err(WebError::already_exists(Resource::User));
    }

    let (email_verified, verification_token, verification_expires) = if state
        .config
        .email
        .as_ref()
        .is_some_and(|e| e.require_verification)
    {
        let token = generate_verification_token();
        let expires = gradient_types::now() + chrono::Duration::hours(24);
        (false, Some(token), Some(expires))
    } else {
        (true, None, None)
    };

    let user = MUser {
        id: UserId::now_v7(),
        username: body.username.clone(),
        name: body.name.to_string(),
        email: body.email.clone(),
        password: Some(generate_hash(body.password.clone())),
        last_login_at: *NULL_TIME,
        created_at: gradient_types::now(),
        email_verified,
        email_verification_token: verification_token.clone(),
        email_verification_token_expires: verification_expires,
        active: true,
        ..Default::default()
    };

    let user = gradient_db::rows::insert::<EUser, _>(&state.web_db, user)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::User))?;

    if let Err(e) = gradient_state::apply_pending_project_memberships(
        &state.web_db,
        &state.memory.pending_project_memberships,
        &user.username,
        user.id,
    )
    .await
    {
        tracing::warn!(
            error = %e,
            username = %user.username,
            "Failed to apply pending state-managed project memberships for new user"
        );
    }

    audit
        .write_as(
            Some(user.id),
            Action::Register,
            EventOwner::default(),
            serde_json::json!({ "username": user.username }),
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
    audit: Audit,
    Json(body): Json<MakeLoginRequest>,
) -> WebResult<Response> {
    if state.config.oidc.as_ref().is_some_and(|o| o.required) {
        return Err(WebError::oauth_required());
    }

    let user = match gradient_db::users::by_username_or_email(
        &state.web_db,
        &body.loginname,
        &body.loginname,
    )
    .await?
    {
        Some(u) => u,
        None => {
            audit
                .write(
                    Action::LoginFailure,
                    EventOwner::default(),
                    serde_json::json!({ "loginname": body.loginname }),
                )
                .await;
            return Err(WebError::invalid_credentials());
        }
    };

    let user_password = user.password.clone().ok_or_else(WebError::oauth_required)?;

    if verify_password(body.password, &user_password).is_err() {
        audit
            .write_as(
                Some(user.id),
                Action::LoginFailure,
                EventOwner::default(),
                None,
            )
            .await;
        return Err(WebError::invalid_credentials());
    }

    if !user.active {
        return Err(SharedRefusal::AccountDeactivated.into());
    }

    if state
        .config
        .email
        .as_ref()
        .is_some_and(|e| e.require_verification)
        && !user.email_verified
    {
        return Err(AuthRefusal::EmailNotVerified.into());
    }

    let use_tls = state.config.server.use_tls;
    let (_session_id, token) = create_session_and_token(
        &state.web_db,
        &state.jwt_secret,
        user.id,
        body.remember_me,
        audit.info().user_agent.clone(),
        audit.info().ip.clone(),
    )
    .await
    .map_err(|_| WebError::failed_to_generate_token())?;

    let user_id = user.id;
    update_last_login(&state.web_db, user)
        .await
        .map_err(|_| WebError::failed_to_update_user())?;

    audit
        .write_as(
            Some(user_id),
            Action::LoginSuccess,
            EventOwner::default(),
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
        HeaderValue::from_str(&cookie).context("bad cookie")?,
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

/// The upstream IdP detail is only logged. A client probing the endpoint is seeing a generic 401.
fn oidc_failure(stage: &'static str) -> impl FnOnce(anyhow::Error) -> WebError {
    move |e| {
        tracing::warn!(stage, error = format!("{:#}", e), "OIDC flow failed");
        WebError::from(AuthRefusal::OidcLoginFailed)
    }
}

pub async fn get_oauth_authorize(
    state: State<Arc<ServerState>>,
    audit: Audit,
    headers: axum::http::HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> WebResult<Response> {
    if state.config.oidc.is_none() {
        return Err(WebError::oauth_disabled());
    }

    let code = query.get("code").ok_or_else(WebError::invalid_oauth_code)?;
    let state_query = query.get("state").ok_or(AuthRefusal::MissingOidcState)?;
    let csrf = cookie_value(&headers, OIDC_CSRF_COOKIE)
        .ok_or(AuthRefusal::MissingOidcCsrfCookie)?
        .to_owned();

    let use_tls = state.config.server.use_tls;
    let user: MUser = oidc_login_verify(&state, code.to_string(), state_query.to_string(), csrf)
        .await
        .map_err(oidc_failure("oauth_authorize_callback"))?;

    let (_session_id, token) = create_session_and_token(
        &state.web_db,
        &state.jwt_secret,
        user.id,
        false,
        audit.info().user_agent.clone(),
        audit.info().ip.clone(),
    )
    .await
    .map_err(|_| WebError::failed_to_generate_token())?;

    audit
        .write_as(
            Some(user.id),
            Action::LoginSuccess,
            EventOwner::default(),
            serde_json::json!({ "method": "oidc" }),
        )
        .await;

    let res = BaseResponse {
        error: false,
        message: token,
    };

    let mut response = Json(res).into_response();
    response.headers_mut().append(
        axum::http::header::SET_COOKIE,
        HeaderValue::from_str(&oidc_csrf_clear_cookie(use_tls)).context("bad cookie")?,
    );
    Ok(response)
}

pub async fn post_oauth_authorize(state: State<Arc<ServerState>>) -> WebResult<Response> {
    if state.config.oidc.is_none() {
        return Err(WebError::oauth_disabled());
    }

    let use_tls = state.config.server.use_tls;
    let req = oidc_login_create(&state.config, &state.http, &state.jwt_secret)
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
            .context("bad cookie")?,
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
    let req = oidc_login_create(&state.config, &state.http, &state.jwt_secret)
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
        .context("build redirect response")
        .map_err(Into::into)
}

pub async fn get_oidc_callback(
    state: State<Arc<ServerState>>,
    audit: Audit,
    headers: axum::http::HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> WebResult<Response> {
    let code = query.get("code").ok_or_else(WebError::invalid_oauth_code)?;
    let state_query = query.get("state").ok_or(AuthRefusal::MissingOidcState)?;
    let csrf = cookie_value(&headers, OIDC_CSRF_COOKIE)
        .ok_or(AuthRefusal::MissingOidcCsrfCookie)?
        .to_owned();

    let use_tls = state.config.server.use_tls;
    let user: MUser = oidc_login_verify(&state, code.to_string(), state_query.to_string(), csrf)
        .await
        .map_err(oidc_failure("oidc_callback"))?;

    let (_session_id, token) = create_session_and_token(
        &state.web_db,
        &state.jwt_secret,
        user.id,
        false,
        audit.info().user_agent.clone(),
        audit.info().ip.clone(),
    )
    .await
    .map_err(|_| WebError::failed_to_generate_token())?;

    audit
        .write_as(
            Some(user.id),
            Action::LoginSuccess,
            EventOwner::default(),
            serde_json::json!({ "method": "oidc" }),
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
        .context("build redirect response")
        .map_err(Into::into)
}

pub async fn post_logout(
    state: State<Arc<ServerState>>,
    audit: Audit,
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

    if let Some(claims) = session_claims(&state.jwt_secret, &headers) {
        let session_id = claims.jti;
        let user_id = claims.id;
        if let Ok(Some(session)) = by_id::<ESession, _>(&state.web_db, session_id).await {
            let _ =
                gradient_db::sessions::revoke(&state.web_db, session, gradient_types::now()).await;
        }

        audit
            .write_as(
                Some(user_id),
                Action::Logout,
                EventOwner::default(),
                serde_json::json!({ "session_id": session_id.to_string() }),
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
        HeaderValue::from_str(&clear_cookie).context("bad cookie")?,
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
        format!("{}; Max-Age=2592000", base)
    } else {
        base
    }
}

pub async fn post_check_username(
    state: State<Arc<ServerState>>,
    Json(body): Json<CheckUsernameRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if let Err(e) = validate_username(&body.username) {
        return Ok(Json(BaseResponse {
            error: true,
            message: e.to_string(),
        }));
    }

    let existing_user = gradient_db::users::by_username(&state.web_db, &body.username).await?;

    if existing_user.is_some() {
        return Ok(Json(BaseResponse {
            error: true,
            message: "Username is already taken".to_string(),
        }));
    }

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
        return Err(AuthRefusal::EmailVerificationDisabled.into());
    }

    let token = query
        .get("token")
        .ok_or(AuthRefusal::MissingVerificationToken)?;

    let user = gradient_db::users::by_email_verification_token(&state.web_db, token)
        .await?
        .ok_or(AuthRefusal::InvalidVerificationToken)?;

    if let Some(expires) = user.email_verification_token_expires
        && gradient_types::now() > expires
    {
        return Err(AuthRefusal::ExpiredVerificationToken.into());
    }

    if user.email_verified {
        return Ok(ok_json("Email already verified".to_string()));
    }

    gradient_db::users::mark_email_verified(&state.web_db, user).await?;

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
        return Err(AuthRefusal::EmailVerificationDisabled.into());
    }

    let user = gradient_db::users::by_username(&state.web_db, &body.username)
        .await?
        .or_not_found(Resource::User)?;

    if user.email_verified {
        return Err(AuthRefusal::EmailAlreadyVerified.into());
    }

    let verification_token = generate_verification_token();
    let verification_expires = gradient_types::now() + chrono::Duration::hours(24);

    let tx = state.web_db.inner().begin().await?;

    gradient_db::users::set_email_verification_token(
        &tx,
        user.clone(),
        verification_token.clone(),
        verification_expires,
    )
    .await?;

    state
        .email
        .send_verification_email(
            &user.email,
            &user.name,
            &verification_token,
            &state.config.server.serve_url,
        )
        .await
        .context("send verification email")?;

    tx.commit().await?;

    Ok(ok_json("Verification email sent successfully".to_string()))
}

const CLI_DEVICE_LIFETIME_MINUTES: i64 = 10;
const CLI_DEVICE_POLL_INTERVAL_SECONDS: u64 = 3;

/// Visually ambiguous characters (0/O, 1/I/L) are omitted. A code typed from a phone screen into a
/// terminal is less likely to be mis-typed.
const CLI_USER_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

fn generate_device_code() -> String {
    Alphanumeric.sample_string(&mut rand::rng(), 64)
}

fn generate_user_code() -> String {
    let mut rng = rand::rng();
    let mut s = String::with_capacity(9);
    for i in 0..8 {
        if i == 4 {
            s.push('-');
        }
        let c = *CLI_USER_CODE_ALPHABET
            .choose(&mut rng)
            .expect("alphabet non-empty");
        s.push(c as char);
    }
    s
}

fn hash_device_code(raw: &str) -> String {
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    hex::encode(h.finalize())
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CliDeviceStartResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: i64,
    pub interval: u64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CliDevicePollRequest {
    pub device_code: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CliDeviceAuthorizeRequest {
    pub user_code: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CliDeviceUserCodeQuery {
    pub user_code: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CliDeviceInfoResponse {
    pub user_code: String,
    pub expires_at: chrono::NaiveDateTime,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
}

pub async fn post_cli_device_start(
    state: State<Arc<ServerState>>,
    audit: Audit,
) -> WebResult<Json<BaseResponse<CliDeviceStartResponse>>> {
    let device_code = generate_device_code();
    let user_code = loop {
        let candidate = generate_user_code();
        let exists = gradient_db::cli_device_authorizations::unexpired_by_user_code(
            &state.web_db,
            &candidate,
            gradient_types::now(),
        )
        .await?;
        if exists.is_none() {
            break candidate;
        }
    };

    let now = gradient_types::now();
    let expires_at = now + Duration::minutes(CLI_DEVICE_LIFETIME_MINUTES);

    let row = MCliDeviceAuthorization {
        id: CliDeviceAuthorizationId::now_v7(),
        device_code_hash: hash_device_code(&device_code),
        user_code: user_code.clone(),
        created_at: now,
        expires_at,
        user_agent: audit.info().user_agent.clone(),
        ip: audit.info().ip.clone(),
        ..Default::default()
    };

    gradient_db::rows::insert::<ECliDeviceAuthorization, _>(&state.web_db, row).await?;

    audit
        .write(
            Action::CliDeviceStart,
            EventOwner::default(),
            serde_json::json!({ "user_code": user_code }),
        )
        .await;

    let base = state.config.server.serve_url.trim_end_matches('/');
    let verification_uri = format!("{}/account/cli-authorize", base);
    let verification_uri_complete = format!("{}?code={}", verification_uri, user_code);

    Ok(ok_json(CliDeviceStartResponse {
        device_code,
        user_code,
        verification_uri,
        verification_uri_complete,
        expires_in: CLI_DEVICE_LIFETIME_MINUTES * 60,
        interval: CLI_DEVICE_POLL_INTERVAL_SECONDS,
    }))
}

pub async fn post_cli_device_poll(
    state: State<Arc<ServerState>>,
    Json(body): Json<CliDevicePollRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let hash = hash_device_code(&body.device_code);
    let row = gradient_db::cli_device_authorizations::by_device_code_hash(&state.web_db, &hash)
        .await?
        .ok_or_else(|| WebError::not_found(Resource::DeviceAuthorization))?;

    let now = gradient_types::now();
    if row.denied_at.is_some() {
        return Err(AuthRefusal::DeviceAuthorizationDenied.into());
    }
    if let Some(token) = row.token.clone() {
        let cleared =
            gradient_db::cli_device_authorizations::clear_token(&state.web_db, row).await?;
        if cleared.is_none() {
            return Err(AuthRefusal::DeviceAuthorizationExpired.into());
        }
        return Ok(ok_json(token));
    }
    if row.authorized_at.is_some() || row.expires_at < now {
        return Err(AuthRefusal::DeviceAuthorizationExpired.into());
    }
    Err(AuthRefusal::DeviceAuthorizationPending.into())
}

async fn find_active_user_code(db: &WebDb, user_code: &str) -> WebResult<MCliDeviceAuthorization> {
    let row = gradient_db::cli_device_authorizations::by_user_code(db, user_code)
        .await?
        .ok_or_else(|| WebError::not_found(Resource::DeviceAuthorization))?;

    let now = gradient_types::now();
    if row.denied_at.is_some() {
        return Err(AuthRefusal::DeviceAuthorizationAlreadyDenied.into());
    }
    if row.authorized_at.is_some() {
        return Err(AuthRefusal::DeviceAuthorizationAlreadyCompleted.into());
    }
    if row.expires_at < now {
        return Err(AuthRefusal::DeviceAuthorizationExpired.into());
    }
    Ok(row)
}

async fn refusal_for_settled_code(db: &WebDb, user_code: &str) -> WebError {
    match find_active_user_code(db, user_code).await {
        Err(refusal) => refusal,
        Ok(_) => AuthRefusal::DeviceAuthorizationAlreadyCompleted.into(),
    }
}

pub async fn get_cli_device_info(
    state: State<Arc<ServerState>>,
    Query(query): Query<CliDeviceUserCodeQuery>,
) -> WebResult<Json<BaseResponse<CliDeviceInfoResponse>>> {
    let row = find_active_user_code(&state.web_db, &query.user_code).await?;
    Ok(ok_json(CliDeviceInfoResponse {
        user_code: row.user_code,
        expires_at: row.expires_at,
        user_agent: row.user_agent,
        ip: row.ip,
    }))
}

pub async fn post_cli_device_authorize(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Json(body): Json<CliDeviceAuthorizeRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let row = find_active_user_code(&state.web_db, &body.user_code).await?;

    let (_session_id, token) = create_session_and_token(
        &state.web_db,
        &state.jwt_secret,
        user.id,
        true,
        row.user_agent.clone(),
        row.ip.clone(),
    )
    .await
    .map_err(|_| WebError::failed_to_generate_token())?;

    let now = gradient_types::now();
    let authorized =
        gradient_db::cli_device_authorizations::authorize(&state.web_db, row, user.id, token, now)
            .await?;
    if authorized.is_none() {
        return Err(refusal_for_settled_code(&state.web_db, &body.user_code).await);
    }

    audit
        .write(
            Action::CliDeviceAuthorize,
            EventOwner::default(),
            serde_json::json!({ "user_code": body.user_code }),
        )
        .await;

    Ok(ok_json("Device authorized".to_string()))
}

pub async fn post_cli_device_deny(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Json(body): Json<CliDeviceAuthorizeRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let row = find_active_user_code(&state.web_db, &body.user_code).await?;

    let now = gradient_types::now();
    if gradient_db::cli_device_authorizations::deny(&state.web_db, row, now)
        .await?
        .is_none()
    {
        return Err(refusal_for_settled_code(&state.web_db, &body.user_code).await);
    }

    audit
        .write(
            Action::CliDeviceDeny,
            EventOwner::default(),
            serde_json::json!({ "user_code": body.user_code }),
        )
        .await;

    Ok(ok_json("Device denied".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
