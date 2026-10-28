/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
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

use super::api_key::{DecodedRequest, MaybeApiKey};
use super::job_scope::{JobScopeRefusal, job_scope_of_request};
use super::jwt::{bearer_token, decode_jwt, extract_bearer_or_cookie, session_cookie_token};
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
    #[error("Unable to decode token")]
    UndecodableToken,
    #[error("User not found")]
    UnknownUser,
}

impl Refusal for Denial {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::UndecodableToken | Self::UnknownUser => RefusalStatus::Unauthorized,
            Self::MalformedAuthorizationHeader | Self::NoCredentials => RefusalStatus::Forbidden,
        }
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

    let Ok(decoded) = decode_jwt(
        &state.web_db,
        &state.jwt_secret,
        &state.memory.last_used_stamps,
        token_str,
    )
    .await
    else {
        return Err(deny(&audit, None, method, path, Denial::UndecodableToken).await);
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

    let Some(current_user) = by_id::<EUser, _>(&state.web_db, user_id).await? else {
        let denial = Denial::UnknownUser;
        return Err(deny(&audit, Some(user_id), method, path, denial).await);
    };

    if !current_user.active {
        let denial = SharedRefusal::AccountDeactivated;
        return Err(deny(&audit, Some(user_id), method, path, denial).await);
    }

    parts.extensions.insert(current_user);
    parts.extensions.insert(MaybeApiKey::from(&decoded));
    parts.extensions.insert(decoded.principal());
    parts.extensions.insert(ClientIp(client_ip));
    Ok(next.run(Request::from_parts(parts, body)).await)
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
) -> Option<(MUser, DecodedRequest)> {
    let token = extract_bearer_or_cookie(headers)?.to_owned();
    let decoded = decode_jwt(db, jwt_secret, last_used_stamps, token)
        .await
        .ok()?;
    let allowed_from_here = decoded
        .api_key_context()
        .is_none_or(|context| ip_allowed(client_ip, &context.allowed_ips));

    if !allowed_from_here {
        return None;
    }

    let user = by_id::<EUser, _>(db, decoded.user_id())
        .await
        .ok()
        .flatten()
        .filter(|user| user.active)?;

    Some((user, decoded))
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

    let (mut parts, body) = req.into_parts();
    match caller {
        Some((user, decoded)) => {
            if let Err(refusal) = job_scope_of_caller(&mut parts, &decoded).await {
                return WebError::from(refusal).into_response();
            }
            parts.extensions.insert(MaybeUser(Some(user)));
            parts.extensions.insert(MaybeApiKey::from(&decoded));
            parts.extensions.insert(decoded.principal());
        }
        None => {
            parts.extensions.insert(MaybeUser(None));
            parts.extensions.insert(MaybeApiKey::none());
        }
    }

    parts.extensions.insert(ClientIp(client_ip));
    next.run(Request::from_parts(parts, body)).await
}

pub async fn update_last_login(db: &WebDb, user: MUser) -> Result<MUser> {
    gradient_db::users::stamp_last_login(db, user, gradient_types::now())
        .await
        .context("update user last login")
}
