/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::auth::{csrf_clear_cookie, csrf_set_cookie, jwt_cookie};
use crate::audit::Audit;
use crate::authorization::{
    ConnectRefusal, PasswordLogin, ProviderCallback, ProviderPurpose, can_disconnect,
    connect_identity, cookie_value, create_session_and_token, provider_authorize,
    provider_callback, session_claims, sign_in_external,
};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::refusal::{Refusal, RefusalStatus};
use anyhow::Context;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::{Extension, Json};
use gradient_core::{ExternalIdentity, LoginMethods, LoginProvider, LoginRefusal, ServerState};
use gradient_db::WebDb;
use gradient_state::PendingProjectMemberships;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use std::collections::HashMap;
use std::sync::Arc;

pub(super) const LOGIN_CSRF_COOKIE: &str = "login_csrf";

#[derive(Debug, thiserror::Error)]
enum ProviderRefusal {
    #[error("Unknown login provider")]
    UnknownProvider,
    #[error("Login failed")]
    LoginFailed,
    #[error("No account of this provider is connected")]
    NotConnected,
    #[error("Add a password or another login method before disconnecting this account")]
    LastLoginMethod,
    #[error("Connecting an account needs a signed-in browser session")]
    SessionRequired,
}

impl Refusal for ProviderRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::LastLoginMethod => RefusalStatus::Conflict,
            Self::UnknownProvider | Self::NotConnected => RefusalStatus::NotFound,
            Self::LoginFailed | Self::SessionRequired => RefusalStatus::Unauthorized,
        }
    }
}

fn find_provider(methods: &LoginMethods, id: &str) -> WebResult<Arc<dyn LoginProvider>> {
    methods
        .provider(id)
        .cloned()
        .ok_or_else(|| ProviderRefusal::UnknownProvider.into())
}

fn provider_redirect(
    config: &RuntimeConfig,
    jwt_secret: &SecretString,
    provider: &dyn LoginProvider,
    purpose: ProviderPurpose,
) -> WebResult<Response> {
    let req = provider_authorize(config, jwt_secret, provider, purpose).map_err(|e| {
        tracing::warn!(
            provider = provider.id(),
            error = format!("{:#}", e),
            "login start failed"
        );
        WebError::from(ProviderRefusal::LoginFailed)
    })?;
    Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", req.auth_url.to_string())
        .header(
            "Set-Cookie",
            csrf_set_cookie(LOGIN_CSRF_COOKIE, &req.cookie_value, config.server.use_tls),
        )
        .body(Body::empty())
        .context("build redirect response")
        .map_err(Into::into)
}

#[utoipa::path(
    get,
    path = "/auth/providers/{provider}/login",
    operation_id = "providerLogin",
    tag = "auth",
    summary = "Login provider redirect",
    description = "Redirects the browser to the login provider (HTTP 302) using PKCE (S256) and sets a\nshort-lived `login_csrf` cookie carrying the signed `state`, the provider and the PKCE verifier.",
    params(("provider" = String, Path, description = "Login provider ID from `GET /config`")),
    responses((status = FOUND, description = "Redirect to the login provider"), WebError)
)]
pub async fn get_provider_login(
    state: State<Arc<ServerState>>,
    Path(provider): Path<String>,
) -> WebResult<Response> {
    let provider = find_provider(&state.login_methods(), &provider)?;
    provider_redirect(
        &state.config,
        &state.jwt_secret,
        provider.as_ref(),
        ProviderPurpose::Login,
    )
}

#[utoipa::path(
    get,
    path = "/auth/providers/{provider}/callback",
    operation_id = "providerCallback",
    tag = "auth",
    summary = "Login provider callback",
    description = "Handles the provider redirect for both sign-in and connecting an account. Verifies the\n`state` parameter against the `login_csrf` cookie and asks the provider for the identity.\nA sign-in sets the `jwt_token` session cookie and redirects to the frontend. A connection\nredirects to the profile settings page. Failures redirect to the login page with a\n`login_error` code.",
    params(
        ("provider" = String, Path, description = "Login provider ID from `GET /config`"),
        ("code" = String, Query),
        ("state" = String, Query)
    ),
    responses((status = FOUND, description = "Redirect to the frontend"), WebError)
)]
pub async fn get_provider_callback(
    state: State<Arc<ServerState>>,
    audit: Audit,
    headers: HeaderMap,
    Path(provider): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> WebResult<Response> {
    let provider = find_provider(&state.login_methods(), &provider)?;
    let use_tls = state.config.server.use_tls;
    let outcome: Result<(String, Option<String>), &'static str> = async {
        let (Some(code), Some(state_query), Some(csrf)) = (
            query.get("code"),
            query.get("state"),
            cookie_value(&headers, LOGIN_CSRF_COOKIE),
        ) else {
            return Err("failed");
        };
        let callback = ProviderCallback {
            code,
            state: state_query,
            csrf_cookie: csrf,
        };
        let (purpose, identity) = provider_callback(
            &state.config,
            &state.http,
            &state.jwt_secret,
            provider.as_ref(),
            callback,
        )
        .await
        .map_err(refusal_code("provider_callback"))?;

        match purpose {
            ProviderPurpose::Login => {
                let token = sign_in(
                    &state.web_db,
                    &state.memory.pending_project_memberships,
                    &state.jwt_secret,
                    PasswordLogin::of(&state.login_methods()),
                    &audit,
                    identity,
                )
                .await?;
                Ok(("/account/oidc-callback".to_owned(), Some(token)))
            }
            ProviderPurpose::Connect {
                user_id,
                session_id,
            } => Ok((
                connect(
                    &state.web_db,
                    &audit,
                    provider.id(),
                    user_id,
                    session_id,
                    &identity,
                )
                .await,
                None,
            )),
        }
    }
    .await;
    let (location, session_token) =
        outcome.unwrap_or_else(|code| (format!("/account/login?login_error={code}"), None));

    let mut response = Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", location)
        .header("Set-Cookie", csrf_clear_cookie(LOGIN_CSRF_COOKIE, use_tls));
    if let Some(token) = session_token {
        response = response.header("Set-Cookie", jwt_cookie(&token, false, use_tls));
    }
    response
        .body(Body::empty())
        .context("build redirect response")
        .map_err(Into::into)
}

/// Every callback failure becomes an error code for the login page, since the browser arrives by redirect.
fn refusal_code(stage: &'static str) -> impl FnOnce(anyhow::Error) -> &'static str {
    move |e| match e.downcast_ref::<LoginRefusal>() {
        Some(refusal) => refusal.code(),
        None => {
            tracing::warn!(stage, error = format!("{:#}", e), "provider login failed");
            "failed"
        }
    }
}

async fn sign_in(
    db: &WebDb,
    pending: &PendingProjectMemberships,
    jwt_secret: &SecretString,
    password_login: PasswordLogin,
    audit: &Audit,
    identity: ExternalIdentity,
) -> Result<String, &'static str> {
    let user = sign_in_external(db, pending, identity, password_login)
        .await
        .map_err(refusal_code("provider_sign_in"))?;

    let (_session_id, token) = create_session_and_token(
        db,
        jwt_secret,
        user.id,
        false,
        audit.info().user_agent.clone(),
        audit.info().ip.clone(),
    )
    .await
    .map_err(|_| "failed")?;

    audit
        .write_as(
            Some(user.id),
            Action::LoginSuccess,
            EventOwner::default(),
            serde_json::json!({ "method": "provider" }),
        )
        .await;
    Ok(token)
}

async fn connect(
    db: &WebDb,
    audit: &Audit,
    provider: &str,
    user_id: UserId,
    session_id: SessionId,
    identity: &ExternalIdentity,
) -> String {
    let outcome = match connect_identity(db, user_id, session_id, identity).await {
        Ok(()) => {
            audit
                .write_as(
                    Some(user_id),
                    Action::IdentityConnect,
                    EventOwner::default(),
                    serde_json::json!({ "provider": provider }),
                )
                .await;
            return "/settings/profile?connected=1".to_owned();
        }
        Err(e) => e,
    };
    let code = match outcome.downcast_ref::<ConnectRefusal>() {
        Some(refusal) => refusal.query_code(),
        None => {
            tracing::warn!(
                error = format!("{:#}", outcome),
                "connecting an account failed"
            );
            "failed"
        }
    };
    format!("/settings/profile?connect_error={code}")
}

#[utoipa::path(
    get,
    path = "/user/identities/{provider}/connect",
    operation_id = "providerConnect",
    tag = "user",
    summary = "Connect a login provider account",
    description = "Redirects the signed-in user to the login provider (HTTP 302). The signed `login_csrf`\ncookie carries the user and the session, and the callback attaches the account to this user\nonly while that session is still active. API keys cannot call this endpoint.",
    params(("provider" = String, Path, description = "Login provider ID from `GET /config`")),
    responses((status = FOUND, description = "Redirect to the login provider"), WebError)
)]
pub async fn get_provider_connect(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    headers: HeaderMap,
    Path(provider): Path<String>,
) -> WebResult<Response> {
    let provider = find_provider(&state.login_methods(), &provider)?;
    let session = session_claims(&state.jwt_secret, &headers)
        .filter(|claims| claims.id == user.id)
        .ok_or(ProviderRefusal::SessionRequired)?;
    provider_redirect(
        &state.config,
        &state.jwt_secret,
        provider.as_ref(),
        ProviderPurpose::Connect {
            user_id: user.id,
            session_id: session.jti,
        },
    )
}

#[utoipa::path(
    delete,
    path = "/user/identities/{provider}",
    operation_id = "providerDisconnect",
    tag = "user",
    summary = "Disconnect a login provider account",
    description = "Removes the connected account of the login provider. Refused when no other login method\nwould work under the current configuration. API keys cannot call this endpoint.",
    params(("provider" = String, Path, description = "Login provider ID from `GET /config`")),
    responses((status = OK, description = "Account disconnected", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_provider_identity(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Path(provider): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let provider = find_provider(&state.login_methods(), &provider)?;
    let identities = gradient_db::user_identities::of_user(&state.web_db, user.id).await?;
    if !identities.iter().any(|i| i.issuer == provider.issuer()) {
        return Err(ProviderRefusal::NotConnected.into());
    }
    let methods = state.login_methods();
    if !can_disconnect(
        &methods,
        user.password.is_some(),
        &identities,
        provider.issuer(),
    ) {
        return Err(ProviderRefusal::LastLoginMethod.into());
    }
    gradient_db::user_identities::delete(&state.web_db, user.id, provider.issuer()).await?;
    audit
        .write(
            Action::IdentityDisconnect,
            EventOwner::default(),
            serde_json::json!({ "provider": provider.id() }),
        )
        .await;
    Ok(ok_json("Account disconnected".to_owned()))
}
