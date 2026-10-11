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

use gradient_core::ServerState;
use gradient_types::*;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use super::api_key::MaybeApiKey;
use super::jwt::{bearer_token, decode_jwt, extract_bearer_or_cookie, session_cookie_token};
use crate::audit::{RequestInfo, record as audit_record};
use crate::client_ip::{ClientIp, resolve_client_ip};
use crate::error::{WebError, WebResult};
use crate::ip_allowlist::is_allowed as ip_allowed;
use crate::refusal::{Refusal, RefusalStatus, SharedRefusal};
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
    state: &Arc<ServerState>,
    user_id: Option<UserId>,
    info: RequestInfo,
    method: String,
    path: String,
    denial: impl Refusal,
) -> WebError {
    audit_record(
        state,
        user_id,
        Action::AuthDeny,
        EventOwner::default(),
        &info,
        Some(serde_json::json!({
            "reason": denial.to_string(),
            "method": method,
            "path": path,
        })),
    )
    .await;

    denial.into()
}

pub async fn authorize(
    state: State<Arc<ServerState>>,
    mut req: Request,
    next: Next,
) -> WebResult<Response<Body>> {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));

    let info =
        RequestInfo::from_request(req.headers(), peer, &state.config.network.trusted_proxies);

    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();

    let presented = presented_token(req.headers()).map(str::to_owned);
    let token_str = match presented {
        Ok(token) => token,
        Err(denial) => return Err(deny(&state, None, info, method, path, denial).await),
    };

    let Ok(decoded) = decode_jwt(state.clone(), token_str).await else {
        return Err(deny(&state, None, info, method, path, Denial::UndecodableToken).await);
    };

    let user_id = decoded.user_id();
    let client_ip = resolve_client_ip(req.headers(), peer, &state.config.network.trusted_proxies);
    let api_key_context = decoded.api_key_context();
    if api_key_context.is_some_and(|context| !ip_allowed(client_ip, &context.allowed_ips)) {
        let denial = SharedRefusal::SourceIpNotAllowed;
        return Err(deny(&state, Some(user_id), info, method, path, denial).await);
    }

    let api_key_extension = MaybeApiKey::from(&decoded);
    let Some(current_user) = EUser::find_by_id(user_id).one(&state.web_db).await? else {
        let denial = Denial::UnknownUser;
        return Err(deny(&state, Some(user_id), info, method, path, denial).await);
    };

    if !current_user.active {
        let denial = SharedRefusal::AccountDeactivated;
        return Err(deny(&state, Some(user_id), info, method, path, denial).await);
    }

    req.extensions_mut().insert(current_user);
    req.extensions_mut().insert(api_key_extension);
    req.extensions_mut().insert(ClientIp(client_ip));
    Ok(next.run(req).await)
}

async fn optional_caller(
    state: &Arc<ServerState>,
    headers: &HeaderMap,
    client_ip: IpAddr,
) -> Option<(MUser, MaybeApiKey)> {
    let token = extract_bearer_or_cookie(headers)?.to_owned();
    let decoded = decode_jwt(State(Arc::clone(state)), token).await.ok()?;
    let allowed_from_here = decoded
        .api_key_context()
        .is_none_or(|context| ip_allowed(client_ip, &context.allowed_ips));

    if !allowed_from_here {
        return None;
    }

    let user = EUser::find_by_id(decoded.user_id())
        .one(&state.web_db)
        .await
        .ok()
        .flatten()
        .filter(|user| user.active)?;

    Some((user, MaybeApiKey::from(&decoded)))
}

pub async fn authorize_optional(
    state: State<Arc<ServerState>>,
    mut req: Request,
    next: Next,
) -> Response<Body> {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));

    let client_ip = resolve_client_ip(req.headers(), peer, &state.config.network.trusted_proxies);
    let (maybe_user, maybe_api_key) = optional_caller(&state, req.headers(), client_ip)
        .await
        .map_or((None, MaybeApiKey::none()), |(user, api_key)| {
            (Some(user), api_key)
        });

    req.extensions_mut().insert(MaybeUser(maybe_user));
    req.extensions_mut().insert(maybe_api_key);
    req.extensions_mut().insert(ClientIp(client_ip));
    next.run(req).await
}

pub async fn update_last_login(state: State<Arc<ServerState>>, user: MUser) -> Result<MUser> {
    let mut auser: AUser = user.into();

    auser.last_login_at = Set(gradient_types::now());
    auser
        .update(&state.web_db)
        .await
        .context("update user last login")
}
