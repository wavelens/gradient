/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use gradient_db::WebDb;
use gradient_db::lookup::by_id;
use gradient_util::debounce::Debounce;
use uuid::Uuid;

use gradient_core::ServerState;
use gradient_types::*;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use super::api_key::{ApiKeyContext, DecodedRequest, MaybeApiKey};
use super::cache_or_project_restricted_api_key_route_access::refuse_request_outside_api_key_scope;
use super::job_scope::{JobScopeRefusal, job_scope_of_request};
use super::jwt::{
    bearer_token, clear_jwt_token_cookie, decode_jwt, extend_session_and_return_set_cookie_if_due,
    extract_bearer_or_cookie, session_cookie_token,
};
use crate::audit::{Audit, RequestInfo};
use crate::client_ip::{ClientIp, resolve_client_ip};
use crate::error::{WebError, WebResult};
use crate::ip_allowlist::is_allowed as ip_allowed;
use crate::refusal::{Refusal, RefusalStatus, SharedRefusal};
use axum::http::request::Parts;
use axum::response::IntoResponse;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;

#[derive(Clone)]
pub struct MaybeUser(pub Option<MUser>);

#[derive(Clone, Copy, Debug, thiserror::Error)]
enum Denial {
    #[error("Invalid Authorization header")]
    MalformedAuthorizationHeader,
    #[error("Authorization header not found")]
    NoCredentials,
    #[error("Your session has ended or the API key is no longer valid. Sign in again.")]
    UndecodableToken,
    #[error("User not found")]
    UnknownUser,
    #[error("Access this endpoint with a signed-in session. API keys don't work.")]
    ApiKeyOnSessionOnlyRoute,
}

impl Refusal for Denial {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::UndecodableToken | Self::UnknownUser => RefusalStatus::Unauthorized,
            Self::MalformedAuthorizationHeader
            | Self::NoCredentials
            | Self::ApiKeyOnSessionOnlyRoute => RefusalStatus::Forbidden,
        }
    }
}

pub async fn require_session_authentication(req: Request, next: Next) -> WebResult<Response<Body>> {
    let via_api_key = req
        .extensions()
        .get::<MaybeApiKey>()
        .is_none_or(|api_key| api_key.as_ref().is_some());
    if via_api_key {
        return Err(Denial::ApiKeyOnSessionOnlyRoute.into());
    }
    Ok(next.run(req).await)
}

pub fn reject_api_key_authentication(api_key: Option<&ApiKeyContext>) -> WebResult<()> {
    match api_key {
        Some(_) => Err(Denial::ApiKeyOnSessionOnlyRoute.into()),
        None => Ok(()),
    }
}

fn token_decode_status_to_web_error(status: StatusCode) -> WebError {
    if status == StatusCode::INTERNAL_SERVER_ERROR {
        anyhow::anyhow!("Failed to look up session or API key in the database").into()
    } else {
        Denial::UndecodableToken.into()
    }
}

fn presented_token(headers: &HeaderMap) -> Result<&str, Denial> {
    if headers.contains_key(AUTHORIZATION) {
        bearer_token(headers).ok_or(Denial::MalformedAuthorizationHeader)
    } else {
        session_cookie_token(headers).ok_or(Denial::NoCredentials)
    }
}

async fn deny(
    audit: &Audit,
    user_id: Option<UserId>,
    method: String,
    path: String,
    denial: impl Refusal,
) -> WebError {
    audit
        .write_as(
            user_id,
            Action::AuthDeny,
            EventOwner::default(),
            serde_json::json!({
                "reason": denial.to_string(),
                "method": method,
                "path": path,
            }),
        )
        .await;

    denial.into()
}

pub async fn authorize(
    state: State<Arc<ServerState>>,
    req: Request,
    next: Next,
) -> WebResult<Response<Body>> {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));

    let info =
        RequestInfo::from_request(req.headers(), peer, &state.config.network.trusted_proxies);
    let audit = Audit::new(state.db(), info);

    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();

    let presented = presented_token(req.headers()).map(str::to_owned);
    let token_str = match presented {
        Ok(token) => token,
        Err(denial) => return Err(deny(&audit, None, method, path, denial).await),
    };

    let decoded = match decode_jwt(
        &state.web_db,
        &state.jwt_secret,
        &state.memory.last_used_stamps,
        token_str,
    )
    .await
    {
        Ok(decoded) => decoded,
        Err(StatusCode::UNAUTHORIZED) => {
            return Err(deny(&audit, None, method, path, Denial::UndecodableToken).await);
        }
        Err(status) => return Err(token_decode_status_to_web_error(status)),
    };

    let user_id = decoded.user_id();
    let client_ip = resolve_client_ip(req.headers(), peer, &state.config.network.trusted_proxies);
    let api_key_context = decoded.api_key_context();
    if api_key_context.is_some_and(|context| !ip_allowed(client_ip, &context.allowed_ips)) {
        let denial = SharedRefusal::SourceIpNotAllowed;
        return Err(deny(&audit, Some(user_id), method, path, denial).await);
    }

    let (mut parts, body) = req.into_parts();
    if let Err(denial) = job_scope_of_caller(&mut parts, &decoded).await {
        return Err(deny(&audit, Some(user_id), method, path, denial).await);
    }
    if let Err(denial) = refuse_request_outside_api_key_scope(&parts, api_key_context) {
        return Err(deny(&audit, Some(user_id), method, path, denial).await);
    }

    let Some(current_user) = by_id::<EUser, _>(&state.web_db, user_id).await? else {
        let denial = Denial::UnknownUser;
        return Err(deny(&audit, Some(user_id), method, path, denial).await);
    };

    if !current_user.active {
        let denial = SharedRefusal::AccountDeactivated;
        return Err(deny(&audit, Some(user_id), method, path, denial).await);
    }

    let renewed_session_cookie = extend_session_and_return_set_cookie_if_due(
        &state.web_db,
        &state.jwt_secret,
        &decoded,
        &parts.headers,
        state.config.server.use_tls,
    )
    .await;

    if let DecodedRequest::Session { session } = &decoded {
        parts.extensions.insert(session.clone());
    }
    parts.extensions.insert(current_user);
    parts.extensions.insert(MaybeApiKey::from(&decoded));
    parts.extensions.insert(decoded.principal());
    parts.extensions.insert(decoded.rate_limit_bucket());
    parts.extensions.insert(ClientIp(client_ip));
    let mut response = next.run(Request::from_parts(parts, body)).await;
    if let Some(cookie) = renewed_session_cookie {
        response.headers_mut().append(SET_COOKIE, cookie);
    }
    Ok(response)
}

async fn job_scope_of_caller(
    parts: &mut Parts,
    decoded: &DecodedRequest,
) -> Result<(), JobScopeRefusal> {
    match decoded.job() {
        Some(job) => job_scope_of_request(parts, job).await,
        None => Ok(()),
    }
}

async fn optional_caller(
    db: &WebDb,
    jwt_secret: &SecretString,
    last_used_stamps: &Debounce<Uuid>,
    headers: &HeaderMap,
    client_ip: IpAddr,
) -> WebResult<Option<(MUser, DecodedRequest)>> {
    let Some(token) = extract_bearer_or_cookie(headers) else {
        return Ok(None);
    };
    let decoded = decode_jwt(db, jwt_secret, last_used_stamps, token.to_owned())
        .await
        .map_err(token_decode_status_to_web_error)?;
    let allowed_from_here = decoded
        .api_key_context()
        .is_none_or(|context| ip_allowed(client_ip, &context.allowed_ips));

    if !allowed_from_here {
        return Err(SharedRefusal::SourceIpNotAllowed.into());
    }

    let user = by_id::<EUser, _>(db, decoded.user_id())
        .await?
        .ok_or(Denial::UnknownUser)?;
    if !user.active {
        return Err(SharedRefusal::AccountDeactivated.into());
    }

    Ok(Some((user, decoded)))
}

fn optional_auth_error_to_response_and_delete_session_cookie(
    error: WebError,
    headers: &HeaderMap,
    use_tls: bool,
) -> Response<Body> {
    let from_cookie = bearer_token(headers).is_none();
    let mut response = error.into_response();
    if from_cookie
        && response.status() == axum::http::StatusCode::UNAUTHORIZED
        && let Ok(cookie) = HeaderValue::from_str(&clear_jwt_token_cookie(use_tls))
    {
        response.headers_mut().insert(SET_COOKIE, cookie);
    }
    response
}

pub async fn authorize_optional(
    state: State<Arc<ServerState>>,
    req: Request,
    next: Next,
) -> Response<Body> {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));

    let client_ip = resolve_client_ip(req.headers(), peer, &state.config.network.trusted_proxies);
    let caller = optional_caller(
        &state.web_db,
        &state.jwt_secret,
        &state.memory.last_used_stamps,
        req.headers(),
        client_ip,
    )
    .await;
    let caller = match caller {
        Ok(caller) => caller,
        Err(error) => {
            return optional_auth_error_to_response_and_delete_session_cookie(
                error,
                req.headers(),
                state.config.server.use_tls,
            );
        }
    };

    let (mut parts, body) = req.into_parts();
    let mut renewed_session_cookie = None;
    match caller {
        Some((user, decoded)) => {
            if let Err(refusal) = job_scope_of_caller(&mut parts, &decoded).await {
                return WebError::from(refusal).into_response();
            }
            if let Err(refusal) =
                refuse_request_outside_api_key_scope(&parts, decoded.api_key_context())
            {
                return WebError::from(refusal).into_response();
            }
            renewed_session_cookie = extend_session_and_return_set_cookie_if_due(
                &state.web_db,
                &state.jwt_secret,
                &decoded,
                &parts.headers,
                state.config.server.use_tls,
            )
            .await;
            parts.extensions.insert(MaybeUser(Some(user)));
            parts.extensions.insert(MaybeApiKey::from(&decoded));
            parts.extensions.insert(decoded.principal());
            parts.extensions.insert(decoded.rate_limit_bucket());
        }
        None => {
            parts.extensions.insert(MaybeUser(None));
            parts.extensions.insert(MaybeApiKey::none());
        }
    }

    parts.extensions.insert(ClientIp(client_ip));
    let mut response = next.run(Request::from_parts(parts, body)).await;
    if let Some(cookie) = renewed_session_cookie {
        response.headers_mut().append(SET_COOKIE, cookie);
    }
    response
}

pub async fn update_last_login(db: &WebDb, user: MUser) -> Result<MUser> {
    gradient_db::users::stamp_last_login(db, user, gradient_types::now())
        .await
        .context("update user last login")
}
