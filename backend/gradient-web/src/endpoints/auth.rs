/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::audit::Audit;
use crate::authorization::{
    clear_jwt_token_cookie, cookie_value,
    create_session_and_token_with_locked_authorization_recheck,
    credential_insert_error_to_http_error, jwt_cookie, oidc_login_create, oidc_login_verify,
    renew_session_expiry_and_sign_token, session_claims, update_last_login,
};
use crate::error::{ErrorCode, WebError, WebResult};
use crate::extractors::{JsonBody, Query};
use crate::helpers::{OptionExt, ok_json};
use crate::refusal::{Refusal, RefusalStatus, SharedRefusal};
use crate::resource::Resource;
use anyhow::Context;
use axum::Extension;
use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use gradient_db::WebDb;
use gradient_db::lookup::by_id;
use gradient_db::users::VerifiedFactForSessionOrCredentialCreation;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;

use crate::client_ip::{ClientIp, rate_limit_key_for_client_ip};
use crate::password_hashing_and_wrong_sign_in_attempts::{
    WrongPasswordAttemptCountsAndLastAttemptTimesByUserId,
    acquire_password_hashing_slot_with_five_second_timeout,
    compute_argon2_hash_for_new_password_on_blocking_pool_with_limited_semaphore_permit,
    password_matches_account_hash_or_dummy_hash,
};
use chrono::Duration;
pub use gradient_api_types::{
    CheckUsernameRequest, CliDevicePollRequest, CliDeviceStartResponse, MakeLoginRequest,
    MakeUserRequest,
};
use gradient_core::ServerState;
use gradient_notify::{EmailSender, generate_token};
use gradient_types::input::{InputError, validate_password, validate_username};
use gradient_types::*;
use rand::distr::{Alphanumeric, SampleString};
use rand::seq::IndexedRandom;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::IpAddr;
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

#[utoipa::path(
    post,
    path = "/auth/basic/register",
    operation_id = "registerUser",
    tag = "auth",
    summary = "Register a new user",
    description = "Create a new user account. Registration requests fail in OIDC-only mode. They also fail when a username is already in use. Verification emails go out when email verification is on. Users cannot log in until confirmation of the address. Registration requests for an email address linked to an existing account return the same response as a successful registration. Notifications go to that address instead. Response fields reveal no information about which addresses have accounts. Requests that use an email address already in use fail with `email_address_already_in_use` when email verification is off.",
    request_body = MakeUserRequest,
    responses((status = OK, description = "User created", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_basic_register(
    state: State<Arc<ServerState>>,
    audit: Audit,
    JsonBody(body): JsonBody<MakeUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if !state.config.registration.enable || !state.login_methods().password_login() {
        return Err(WebError::registration_disabled());
    }

    let new_user = gradient_db::users::ValidatedNewUser::new(
        AccountOrigin::PublicRegistration,
        &body.username,
        body.name.as_str(),
        Some(&body.email),
    )
    .map_err(|e| match e {
        InputError::EmailAddressNotValid => WebError::invalid_email(),
        e => WebError::invalid_username(e.to_string()),
    })?;

    if body.email.is_empty() {
        return Err(WebError::invalid_email());
    }

    if let Err(e) = validate_password(&body.password) {
        return Err(WebError::invalid_password(e.to_string()));
    }

    if gradient_db::users::by_username(&state.web_db, &body.username)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(Resource::Username));
    }

    let requires_verification = state
        .config
        .email
        .as_ref()
        .is_some_and(|e| e.require_verification);

    let password_hash =
        compute_argon2_hash_for_new_password_on_blocking_pool_with_limited_semaphore_permit(
            body.password.clone(),
        )
        .await?;
    if let Some(owner) = gradient_db::users::by_email(&state.web_db, &body.email).await? {
        if !requires_verification {
            return Err(WebError::already_exists(Resource::Email));
        }
        send_registration_attempt_email_to_existing_account_owner(
            state.email.as_ref(),
            &state.config.server.serve_url,
            &owner,
        )
        .await;
        return Ok(
            registration_success_json_response_for_user_id_with_email_verification_message(
                UserId::now_v7(),
                requires_verification,
            ),
        );
    }

    let verification_token = requires_verification.then(generate_token);
    let new_user = new_user.with_password_hash(Some(password_hash));
    let new_user = match &verification_token {
        Some(token) => new_user.with_pending_email_verification_token_and_expiry(
            token.clone(),
            gradient_types::now() + chrono::Duration::hours(24),
        ),
        None => new_user.with_email_verified(true),
    };

    let user = gradient_db::users::insert_user_account(&state.web_db, new_user)
        .await
        .map_err(|e| WebError::from_db_err(e, Resource::Username))?;

    audit
        .write_as(
            Some(user.id),
            Action::Register,
            EventOwner::default(),
            serde_json::json!({ "username": user.username }),
        )
        .await;

    if requires_verification
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

    Ok(
        registration_success_json_response_for_user_id_with_email_verification_message(
            user.id,
            requires_verification,
        ),
    )
}

fn registration_success_json_response_for_user_id_with_email_verification_message(
    id: UserId,
    requires_verification: bool,
) -> Json<BaseResponse<String>> {
    let message = if requires_verification {
        format!("User {id} created. Please check your email to verify your account.")
    } else {
        format!("User {id} created successfully.")
    };
    Json(BaseResponse {
        error: false,
        message,
    })
}

async fn send_registration_attempt_email_to_existing_account_owner(
    email: &dyn EmailSender,
    serve_url: &str,
    owner: &MUser,
) {
    let body = format!(
        "Hello {name},\n\nSomeone tried to create a new Gradient account with this email address. The account with username {username} already had this address. The signup created no new account.\n\nSign in at {url}/account/login.\n\nIgnore this email when someone else made the signup attempt.",
        name = owner.name,
        username = owner.username,
        url = serve_url,
    );
    if let Err(e) = email
        .send_action_mail(
            std::slice::from_ref(&owner.email),
            "Someone tried to sign up for Gradient with this email address",
            &body,
        )
        .await
    {
        tracing::warn!(error = %e, "Failed to send the email about a signup attempt on an address that already has an account");
    }
}

#[utoipa::path(
    post,
    path = "/auth/basic/login",
    operation_id = "loginUser",
    tag = "auth",
    summary = "Login with username/password",
    description = "Verify credentials. Return a signed JWT. Pass the token as ``Authorization: Bearer <token>`` on later requests. Use either a username or an email address for ``loginname``. Set ``remember_me: true`` to get a long-lived token. Sessions expire 24 hours after last use (30 days with ``remember_me``). Browsers refresh cookie ``jwt_token`` automatically while the session is valid. Bearer clients use ``POST /auth/session/renew`` to refresh the token.",
    request_body = MakeLoginRequest,
    responses((status = OK, description = "JWT token", body = String, content_type = "application/json"), WebError)
)]
pub(crate) async fn post_basic_login(
    state: State<Arc<ServerState>>,
    Extension(wrong_password_counts_by_account): Extension<
        Arc<WrongPasswordAttemptCountsAndLastAttemptTimesByUserId>,
    >,
    audit: Audit,
    JsonBody(body): JsonBody<MakeLoginRequest>,
) -> WebResult<Response> {
    if !state.login_methods().password_login() {
        return Err(WebError::oauth_required());
    }

    let user =
        gradient_db::users::find_user_for_password_login(&state.web_db, &body.loginname).await?;
    let password_hashing_slot = acquire_password_hashing_slot_with_five_second_timeout().await?;
    let recorded_password_attempt = if let Some(user) =
        user.as_ref().filter(|user| user.password.is_some())
    {
        Some(
                wrong_password_counts_by_account
                    .record_password_attempt_or_refuse_with_429_when_wait_is_due_unless_client_network_has_same_account_live_session(
                        &state.web_db,
                        user.id,
                        audit.info().ip.as_deref(),
                    )
                    .await?,
            )
    } else {
        None
    };

    let password_matches = password_matches_account_hash_or_dummy_hash(
        password_hashing_slot,
        body.password.clone(),
        user.as_ref().and_then(|u| u.password.clone()),
    )
    .await?;

    let user = match user {
        Some(user) if password_matches => {
            if let Some(recorded_password_attempt) = recorded_password_attempt {
                wrong_password_counts_by_account
                    .remove_wrong_sign_in_attempts_through_recorded_attempt(
                        user.id,
                        recorded_password_attempt,
                    );
            }
            user
        }
        Some(user) => {
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
    let verified_hash = user
        .password
        .clone()
        .ok_or_else(WebError::invalid_credentials)?;
    let (_session_id, token) = create_session_and_token_with_locked_authorization_recheck(
        &state.web_db,
        &state.jwt_secret,
        user.id,
        VerifiedFactForSessionOrCredentialCreation::PasswordHashStillEqual(verified_hash),
        body.remember_me,
        false,
        audit.info(),
    )
    .await?;

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

pub(super) fn csrf_set_cookie(name: &str, value: &str, use_tls: bool) -> String {
    let secure = if use_tls { "; Secure" } else { "" };
    format!("{name}={value}; HttpOnly{secure}; SameSite=Lax; Path=/; Max-Age=600")
}

pub(super) fn csrf_clear_cookie(name: &str, use_tls: bool) -> String {
    let secure = if use_tls { "; Secure" } else { "" };
    format!("{name}=; HttpOnly{secure}; SameSite=Lax; Path=/; Max-Age=0")
}

/// The upstream IdP detail is only logged. A client probing the endpoint is seeing a generic 401.
fn oidc_failure(stage: &'static str) -> impl FnOnce(anyhow::Error) -> WebError {
    move |e| {
        tracing::warn!(stage, error = format!("{:#}", e), "OIDC flow failed");
        WebError::from(AuthRefusal::OidcLoginFailed)
    }
}

#[utoipa::path(
    get,
    path = "/auth/oauth/authorize",
    operation_id = "oauthAuthorizeGet",
    tag = "auth",
    summary = "OAuth2 callback (GET)",
    description = "Handles the authorization code returned by the OAuth2/OIDC provider. Verifies the\n`state` parameter against the `oidc_csrf` cookie set by `POST /auth/oauth/authorize`,\nexchanges the code for tokens, verifies the ID token signature against the provider's\nJWKS, and binds the session to the user identified by `(iss, sub)`.",
    params(("code" = String, Query), ("state" = String, Query)),
    responses((status = OK, description = "JWT token", body = String, content_type = "application/json"), WebError)
)]
pub async fn get_oauth_authorize(
    state: State<Arc<ServerState>>,
    audit: Audit,
    headers: axum::http::HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> WebResult<Response> {
    let use_tls = state.config.server.use_tls;
    if state.config.oidc.is_none() {
        return Err(WebError::oauth_disabled());
    }
    let (code, state_query, csrf) =
        read_oidc_login_callback_code_state_and_csrf_cookie(&headers, &query)?;
    let (user, identity_link) = oidc_login_verify(&state, code, state_query, csrf)
        .await
        .map_err(oidc_failure("oauth_authorize_callback"))?;
    let token = create_oidc_login_session_and_token_with_login_success_audit_log(
        &state.web_db,
        &state.jwt_secret,
        &audit,
        user.id,
        identity_link,
    )
    .await?;

    let res = BaseResponse {
        error: false,
        message: token,
    };

    let mut response = Json(res).into_response();
    response.headers_mut().append(
        axum::http::header::SET_COOKIE,
        HeaderValue::from_str(&csrf_clear_cookie(OIDC_CSRF_COOKIE, use_tls))
            .context("bad cookie")?,
    );
    Ok(response)
}

fn read_oidc_login_callback_code_state_and_csrf_cookie(
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> WebResult<(String, String, String)> {
    let code = query.get("code").ok_or_else(WebError::invalid_oauth_code)?;
    let state_query = query.get("state").ok_or(AuthRefusal::MissingOidcState)?;
    let csrf = cookie_value(headers, OIDC_CSRF_COOKIE).ok_or(AuthRefusal::MissingOidcCsrfCookie)?;
    Ok((code.to_owned(), state_query.to_owned(), csrf.to_owned()))
}

async fn create_oidc_login_session_and_token_with_login_success_audit_log(
    web_db: &WebDb,
    jwt_secret: &SecretString,
    audit: &Audit,
    user: UserId,
    identity_link: VerifiedFactForSessionOrCredentialCreation,
) -> WebResult<String> {
    let (_session_id, token) = create_session_and_token_with_locked_authorization_recheck(
        web_db,
        jwt_secret,
        user,
        identity_link,
        false,
        false,
        audit.info(),
    )
    .await?;

    audit
        .write_as(
            Some(user),
            Action::LoginSuccess,
            EventOwner::default(),
            serde_json::json!({ "method": "oidc" }),
        )
        .await;
    Ok(token)
}

#[utoipa::path(
    post,
    path = "/auth/oauth/authorize",
    operation_id = "oauthAuthorizePost",
    tag = "auth",
    summary = "Get OAuth2 authorization URL",
    description = "Returns the authorization URL to redirect the user to for OIDC login and sets a\nshort-lived `oidc_csrf` cookie carrying the signed `state` and `nonce` values that\nmust be presented on the callback. Requires OIDC to be enabled.",
    responses((status = OK, description = "Authorization URL", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_oauth_authorize(state: State<Arc<ServerState>>) -> WebResult<Response> {
    if state.config.oidc.is_none() {
        return Err(WebError::oauth_disabled());
    }

    let use_tls = state.config.server.use_tls;
    let req = oidc_login_create(
        &state.config,
        &state.admin_configured_endpoint_http_client,
        &state.jwt_secret,
    )
    .await
    .map_err(oidc_failure("oauth_authorize_start"))?;

    let res = BaseResponse {
        error: false,
        message: req.auth_url.to_string(),
    };
    let mut response = Json(res).into_response();
    response.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        HeaderValue::from_str(&csrf_set_cookie(
            OIDC_CSRF_COOKIE,
            &req.cookie_value,
            use_tls,
        ))
        .context("bad cookie")?,
    );
    Ok(response)
}

#[utoipa::path(
    get,
    path = "/auth/oidc/login",
    operation_id = "oidcLogin",
    tag = "auth",
    summary = "OIDC login redirect",
    description = "Redirects the browser to the OIDC provider's authorization endpoint (HTTP 302) using\nPKCE (S256), and sets a short-lived `oidc_csrf` cookie carrying the signed `state`,\n`nonce`, and PKCE verifier that must be returned on the callback.",
    responses((status = FOUND, description = "Redirect to OIDC provider"), WebError)
)]
pub async fn get_oidc_login(state: State<Arc<ServerState>>) -> WebResult<Response> {
    if state.config.oidc.is_none() {
        return Err(WebError::oauth_disabled());
    }

    let use_tls = state.config.server.use_tls;
    let req = oidc_login_create(
        &state.config,
        &state.admin_configured_endpoint_http_client,
        &state.jwt_secret,
    )
    .await
    .map_err(oidc_failure("oidc_login_start"))?;

    Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", req.auth_url.to_string())
        .header(
            "Set-Cookie",
            csrf_set_cookie(OIDC_CSRF_COOKIE, &req.cookie_value, use_tls),
        )
        .body(Body::empty())
        .context("build redirect response")
        .map_err(Into::into)
}

#[utoipa::path(
    get,
    path = "/auth/oidc/callback",
    operation_id = "oidcCallback",
    tag = "auth",
    summary = "OIDC callback",
    description = "Handles the OIDC provider redirect. Verifies the `state` parameter against the\n`oidc_csrf` cookie, exchanges the authorization code for tokens, verifies the ID\ntoken signature using the provider's JWKS (validating `iss`, `aud`, `exp`, and\n`nonce`), and binds the session to the user identified by `(iss, sub)`. Sets the\n`jwt_token` session cookie and clears `oidc_csrf`.",
    params(("code" = String, Query), ("state" = String, Query)),
    responses((status = FOUND, description = "Redirect to frontend with session cookie set"), WebError)
)]
pub async fn get_oidc_callback(
    state: State<Arc<ServerState>>,
    audit: Audit,
    headers: axum::http::HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> WebResult<Response> {
    let use_tls = state.config.server.use_tls;
    if state.config.oidc.is_none() {
        return Err(WebError::oauth_disabled());
    }
    let (code, state_query, csrf) =
        read_oidc_login_callback_code_state_and_csrf_cookie(&headers, &query)?;
    let (user, identity_link) = oidc_login_verify(&state, code, state_query, csrf)
        .await
        .map_err(oidc_failure("oidc_callback"))?;
    let token = create_oidc_login_session_and_token_with_login_success_audit_log(
        &state.web_db,
        &state.jwt_secret,
        &audit,
        user.id,
        identity_link,
    )
    .await?;

    let session_cookie = jwt_cookie(&token, false, use_tls);
    let clear_csrf = csrf_clear_cookie(OIDC_CSRF_COOKIE, use_tls);

    Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", "/account/oidc-callback")
        .header("Set-Cookie", &session_cookie)
        .header("Set-Cookie", &clear_csrf)
        .body(Body::empty())
        .context("build redirect response")
        .map_err(Into::into)
}

#[utoipa::path(
    post,
    path = "/auth/session/renew",
    operation_id = "renewSession",
    tag = "auth",
    summary = "Renew session",
    description = "Extend the expiry of the caller session to 24 hours from now. Use 30 days for a `remember_me` session instead. Send back a new signed JWT for that session. Previous tokens keep working until their own expiry. Bearer clients such as the CLI call this endpoint to stay signed in while they work. Browser cookies refresh automatically. Renewal never extends a session past its hard deadline of 30 days after browser sign-in of the CLI, or 90 days for other sessions. Sign in again after that.",
    responses((status = OK, description = "New session token", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_session_renew(
    state: State<Arc<ServerState>>,
    Extension(session): Extension<MSession>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (_session, token) =
        renew_session_expiry_and_sign_token(&state.web_db, &state.jwt_secret, session)
            .await
            .map_err(|_| WebError::failed_to_generate_token())?;

    Ok(ok_json(token))
}

#[utoipa::path(
    post,
    path = "/auth/logout",
    operation_id = "logout",
    tag = "auth",
    summary = "Logout",
    description = "Calls to this endpoint revoke the session of the presented token (`Authorization: Bearer` header or `jwt_token` cookie). The token and the matching session stop working immediately. Calls to this endpoint clear the `jwt_token` cookie. Calls without a valid token still succeed.",
    responses((status = OK, description = "Logged out", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_logout(
    state: State<Arc<ServerState>>,
    audit: Audit,
    headers: HeaderMap,
) -> WebResult<Response> {
    let clear_cookie = clear_jwt_token_cookie(state.config.server.use_tls);

    if let Some(claims) = session_claims(&state.jwt_secret, &headers) {
        let session_id = claims.jti;
        let user_id = claims.id;
        let now = gradient_types::now();
        if let Ok(Some(session)) = by_id::<ESession, _>(&state.web_db, session_id).await
            && session.is_usable_at(now)
            && session.user_id == user_id
        {
            let _ = gradient_db::sessions::revoke(&state.web_db, session, now).await;
            audit
                .write_as(
                    Some(user_id),
                    Action::Logout,
                    EventOwner::default(),
                    serde_json::json!({ "session_id": session_id.to_string() }),
                )
                .await;
        }
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
    response.headers_mut().append(
        axum::http::header::SET_COOKIE,
        HeaderValue::from_str(&csrf_clear_cookie(
            super::login_providers::LOGIN_CSRF_COOKIE,
            state.config.server.use_tls,
        ))
        .context("bad cookie")?,
    );
    Ok(response)
}

#[utoipa::path(
    post,
    path = "/auth/check-username",
    operation_id = "checkUsername",
    tag = "auth",
    summary = "Check username availability",
    description = "Returns `error: false` if the username is valid and not taken, `error: true` otherwise (non-2xx status is not used - always inspect the `error` field).",
    request_body = CheckUsernameRequest,
    responses((status = OK, description = "Availability result", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_check_username(
    state: State<Arc<ServerState>>,
    JsonBody(body): JsonBody<CheckUsernameRequest>,
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

#[utoipa::path(
    get,
    path = "/auth/verify-email",
    operation_id = "verifyEmail",
    tag = "auth",
    summary = "Verify email address",
    description = "Consumes the one-time token sent in the verification email. Only available when `email_verification_enabled` is true.",
    params(("token" = String, Query)),
    responses((status = OK, description = "Email verified", body = String, content_type = "application/json"), WebError)
)]
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

#[utoipa::path(
    post,
    path = "/auth/resend-verification",
    operation_id = "resendVerification",
    tag = "auth",
    summary = "Resend verification email",
    description = "Issues a fresh verification token and sends a new email. Only available when email verification is enabled.",
    request_body = CheckUsernameRequest,
    responses((status = OK, description = "Email sent", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_resend_verification(
    state: State<Arc<ServerState>>,
    JsonBody(body): JsonBody<CheckUsernameRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
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

    let verification_token = generate_token();
    let verification_expires = gradient_types::now() + chrono::Duration::hours(24);

    gradient_db::users::set_email_verification_token(
        &state.web_db,
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

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct CliDeviceAuthorizeRequest {
    pub user_code: String,
}

#[derive(Serialize, Deserialize, Debug, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CliDeviceUserCodeQuery {
    pub user_code: String,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct CliDeviceInfoResponse {
    pub user_code: String,
    pub expires_at: chrono::NaiveDateTime,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub cli_and_browser_on_same_network: bool,
}

impl CliDeviceInfoResponse {
    fn new(authorization: MCliDeviceAuthorization, browser_ip: IpAddr) -> Self {
        let cli_and_browser_on_same_network = authorization
            .ip
            .as_deref()
            .and_then(|cli_ip| cli_ip.parse::<IpAddr>().ok())
            .is_some_and(|cli_ip| {
                rate_limit_key_for_client_ip(cli_ip) == rate_limit_key_for_client_ip(browser_ip)
            });
        Self {
            user_code: authorization.user_code,
            expires_at: authorization.expires_at,
            user_agent: authorization.user_agent,
            ip: authorization.ip,
            cli_and_browser_on_same_network,
        }
    }
}

#[utoipa::path(
    post,
    path = "/auth/cli/start",
    operation_id = "cliDeviceStart",
    tag = "auth",
    summary = "Start a CLI device-authorization flow",
    description = "Create a `device_code` that the server returns once and treats as opaque, plus a short `user_code` that the CLI shows in the terminal. The user can open `verification_uri` in a browser and enter `user_code` there. The CLI polls `/auth/cli/poll` until approval or denial in the browser.",
    responses((status = OK, description = "Device flow started", body = CliDeviceStartResponse), WebError)
)]
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

    Ok(ok_json(CliDeviceStartResponse {
        device_code,
        user_code,
        verification_uri_complete: verification_uri.clone(),
        verification_uri,
        expires_in: CLI_DEVICE_LIFETIME_MINUTES * 60,
        interval: CLI_DEVICE_POLL_INTERVAL_SECONDS,
    }))
}

#[utoipa::path(
    post,
    path = "/auth/cli/poll",
    operation_id = "cliDevicePoll",
    tag = "auth",
    summary = "Poll a pending CLI device authorization",
    description = "Exchange `device_code` for a session token after user approval of the sign-in in the browser. Create a CLI sign-in session (max 30 days) on the first successful poll and delete the device authorization. Later polls get nothing. Return `cli_auth_pending` before the Authorize click. Return `cli_auth_denied` on denial. Return `cli_auth_expired` after expiry of the device code or invalidation of the browser session that approved the sign-in.",
    request_body = CliDevicePollRequest,
    responses((status = OK, description = "Authorized - session token returned", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_cli_device_poll(
    state: State<Arc<ServerState>>,
    audit: Audit,
    JsonBody(body): JsonBody<CliDevicePollRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let hash = hash_device_code(&body.device_code);
    let row = gradient_db::cli_device_authorizations::by_device_code_hash(&state.web_db, &hash)
        .await?
        .ok_or_else(|| WebError::not_found(Resource::DeviceAuthorization))?;

    let now = gradient_types::now();
    if row.denied_at.is_some() {
        return Err(AuthRefusal::DeviceAuthorizationDenied.into());
    }
    if row.authorized_at.is_none() {
        let refusal = if row.expires_at < now {
            AuthRefusal::DeviceAuthorizationExpired
        } else {
            AuthRefusal::DeviceAuthorizationPending
        };
        return Err(refusal.into());
    }

    let claimed = gradient_db::cli_device_authorizations::delete_authorized_unexpired_cli_device_authorization_by_device_code_hash(
        &state.web_db,
        &hash,
        now,
    )
    .await?;
    let Some((user_id, approving_session)) =
        claimed.and_then(|row| row.user_id.zip(row.approving_session_id))
    else {
        return Err(AuthRefusal::DeviceAuthorizationExpired.into());
    };

    let (session_id, token) = create_session_and_token_with_locked_authorization_recheck(
        &state.web_db,
        &state.jwt_secret,
        user_id,
        VerifiedFactForSessionOrCredentialCreation::ExistingSignInSession(approving_session),
        true,
        true,
        audit.info(),
    )
    .await
    .map_err(|e| {
        if e.code() == ErrorCode::INVALID_CREDENTIALS {
            AuthRefusal::DeviceAuthorizationExpired.into()
        } else {
            e
        }
    })?;

    if let Some(user) = by_id::<EUser, _>(&state.web_db, user_id).await? {
        update_last_login(&state.web_db, user)
            .await
            .map_err(|_| WebError::failed_to_update_user())?;
    }

    audit
        .write_as(
            Some(user_id),
            Action::LoginSuccess,
            EventOwner::default(),
            serde_json::json!({ "method": "cli_device", "session_id": session_id }),
        )
        .await;

    Ok(ok_json(token))
}

async fn find_active_user_code<C: sea_orm::ConnectionTrait>(
    db: &C,
    user_code: &str,
) -> WebResult<MCliDeviceAuthorization> {
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

async fn refusal_for_settled_code<C: sea_orm::ConnectionTrait>(
    db: &C,
    user_code: &str,
) -> WebError {
    match find_active_user_code(db, user_code).await {
        Err(refusal) => refusal,
        Ok(_) => AuthRefusal::DeviceAuthorizationAlreadyCompleted.into(),
    }
}

#[utoipa::path(
    get,
    path = "/auth/cli/info",
    operation_id = "cliDeviceInfo",
    tag = "auth",
    summary = "Look up a pending CLI device authorization",
    description = "Return details of the pending `user_code` for the browser page to show. The details also cover whether the CLI that started the code and the browser sit on the same network (same IPv4 address or same IPv6 /64 prefix).",
    params(CliDeviceUserCodeQuery),
    responses((status = OK, description = "Device info", body = CliDeviceInfoResponse), WebError)
)]
pub async fn get_cli_device_info(
    state: State<Arc<ServerState>>,
    Extension(ClientIp(browser_ip)): Extension<ClientIp>,
    Query(query): Query<CliDeviceUserCodeQuery>,
) -> WebResult<Json<BaseResponse<CliDeviceInfoResponse>>> {
    let row = find_active_user_code(&state.web_db, &query.user_code).await?;
    Ok(ok_json(CliDeviceInfoResponse::new(row, browser_ip)))
}

#[utoipa::path(
    post,
    path = "/auth/cli/authorize",
    operation_id = "cliDeviceAuthorize",
    tag = "auth",
    summary = "Authorize a pending CLI device session",
    description = "Mark `user_code` as a device authorization that the calling browser session approved. No session and no token come back from this call. CLI clients receive their session on the next `/auth/cli/poll` request if the browser session that approved the authorization is still valid at that point.",
    request_body = CliDeviceAuthorizeRequest,
    responses((status = OK, description = "Authorized", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_cli_device_authorize(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Extension(session): Extension<MSession>,
    JsonBody(body): JsonBody<CliDeviceAuthorizeRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let row = find_active_user_code(&state.web_db, &body.user_code).await?;

    let user_code = body.user_code.clone();
    gradient_db::users::insert_credential_after_locking_user_row_and_rechecking_authorisation(
        state.web_db.inner(),
        user.id,
        gradient_db::users::VerifiedFactForSessionOrCredentialCreation::ExistingSignInSession(
            session.id,
        ),
        |tx| {
            Box::pin(async move {
                let now = gradient_types::now();
                match gradient_db::cli_device_authorizations::authorize(
                    tx, row, user.id, session.id, now,
                )
                .await?
                {
                    Some(_) => Ok(()),
                    None => Err(refusal_for_settled_code(tx, &user_code).await),
                }
            })
        },
    )
    .await
    .map_err(|e| credential_insert_error_to_http_error(e, std::convert::identity))?;

    audit
        .write(
            Action::CliDeviceAuthorize,
            EventOwner::default(),
            serde_json::json!({ "user_code": body.user_code }),
        )
        .await;

    Ok(ok_json("Device authorized".to_string()))
}

#[utoipa::path(
    post,
    path = "/auth/cli/deny",
    operation_id = "cliDeviceDeny",
    tag = "auth",
    summary = "Deny a pending CLI device session",
    request_body = CliDeviceAuthorizeRequest,
    responses((status = OK, description = "Denied", body = String, content_type = "application/json"), WebError)
)]
pub async fn post_cli_device_deny(
    state: State<Arc<ServerState>>,
    audit: Audit,
    JsonBody(body): JsonBody<CliDeviceAuthorizeRequest>,
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

    #[tokio::test]
    async fn test_logout_with_revoked_session_token_writes_no_logout_audit_event_and_returns_200() {
        let session_id = SessionId::now_v7();
        let revoked_session = MSession {
            revoked_at: Some(gradient_types::now()),
            ..gradient_fixtures::rows::live_session(session_id)
        };
        let db = gradient_fixtures::mock::mock_db()
            .append_query_results([vec![revoked_session]])
            .into_connection();
        let server = gradient_test_support::web::make_test_server(db.clone());

        server
            .post("/api/v1/auth/logout")
            .add_header(
                "authorization",
                &gradient_test_support::web::bearer(session_id),
            )
            .await
            .assert_status_ok();
        assert!(!format!("{:?}", db.into_transaction_log()).contains("audit_log"));
    }

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
