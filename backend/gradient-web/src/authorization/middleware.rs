/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::middleware::Next;
use axum::response::Response;

use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use super::api_key::MaybeApiKey;
use super::jwt::{decode_jwt, extract_bearer_or_cookie, token_from_cookie};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::client_ip::{ClientIp, resolve_client_ip};
use crate::error::{ErrorCode, WebError, WebResult};
use crate::ip_allowlist::is_allowed as ip_allowed;

/// Extension type for optional authentication.
/// Inserted by `authorize_optional` into every request regardless of whether
/// the caller is logged in.
#[derive(Clone)]
pub struct MaybeUser(pub Option<MUser>);

async fn audit_deny(
    state: &Arc<ServerState>,
    user_id: Option<UserId>,
    info: RequestInfo,
    method: String,
    path: String,
    reason: &'static str,
) {
    audit_record(
        &state.web_db,
        user_id,
        events::AUTH_DENY,
        &info,
        Some(serde_json::json!({
            "reason": reason,
            "method": method,
            "path": path,
        })),
    )
    .await;
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

    let auth_header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .cloned();

    let token_str: String = if let Some(header) = auth_header {
        let val = match header.to_str() {
            Ok(v) => v.to_string(),
            Err(_) => {
                audit_deny(
                    &state,
                    None,
                    info,
                    method,
                    path,
                    "Authorization header empty",
                )
                .await;
                return Err(WebError::forbidden("Authorization header empty"));
            }
        };
        let mut parts = val.split_whitespace();
        let (bearer, token) = (parts.next(), parts.next());
        if bearer != Some("Bearer") || token.is_none() {
            audit_deny(
                &state,
                None,
                info,
                method,
                path,
                "Invalid Authorization header",
            )
            .await;
            return Err(WebError::forbidden("Invalid Authorization header"));
        }
        token.unwrap().to_string()
    } else if let Some(t) = token_from_cookie(&req) {
        t
    } else {
        audit_deny(
            &state,
            None,
            info,
            method,
            path,
            "Authorization header not found",
        )
        .await;
        return Err(WebError::forbidden("Authorization header not found"));
    };

    let decoded = match decode_jwt(state.clone(), token_str).await {
        Ok(t) => t,
        Err(_) => {
            audit_deny(&state, None, info, method, path, "Unable to decode token").await;
            return Err(WebError::unauthorized("Unable to decode token"));
        }
    };

    let user_id = decoded.user_id();
    let client_ip = resolve_client_ip(req.headers(), peer, &state.config.network.trusted_proxies);
    let api_key_extension = match decoded.api_key_context() {
        Some(ctx) => {
            if !ip_allowed(client_ip, &ctx.allowed_ips) {
                audit_deny(
                    &state,
                    Some(user_id),
                    info,
                    method,
                    path,
                    "API key source IP not allowed",
                )
                .await;
                return Err(WebError::forbidden_with(
                    ErrorCode::FORBIDDEN_SOURCE_IP,
                    "API key not allowed from this source IP",
                ));
            }
            MaybeApiKey::from_key(ctx.clone())
        }
        None => MaybeApiKey::none(),
    };

    let current_user = match EUser::find_by_id(user_id).one(&state.web_db).await? {
        Some(u) => u,
        None => {
            audit_deny(&state, Some(user_id), info, method, path, "User not found").await;
            return Err(WebError::unauthorized("User not found"));
        }
    };

    req.extensions_mut().insert(current_user);
    req.extensions_mut().insert(api_key_extension);
    req.extensions_mut().insert(ClientIp(client_ip));
    Ok(next.run(req).await)
}

/// Middleware that attempts to authenticate the caller but never rejects the
/// request.  Handlers receive `Extension(MaybeUser(maybe_user))` where
/// `maybe_user` is `Some(user)` for authenticated callers and `None` for
/// unauthenticated ones.
pub async fn authorize_optional(
    state: State<Arc<ServerState>>,
    mut req: Request,
    next: Next,
) -> Response<Body> {
    let mut maybe_user: Option<MUser> = None;
    let mut maybe_api_key = MaybeApiKey::none();

    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    let client_ip =
        resolve_client_ip(req.headers(), peer, &state.config.network.trusted_proxies);

    if let Some(token_str) = extract_bearer_or_cookie(req.headers())
        && let Ok(decoded) = decode_jwt(State(Arc::clone(&state)), token_str).await
    {
        let ip_ok = match decoded.api_key_context() {
            Some(ctx) => ip_allowed(client_ip, &ctx.allowed_ips),
            None => true,
        };
        if ip_ok {
            if let Some(ctx) = decoded.api_key_context() {
                maybe_api_key = MaybeApiKey::from_key(ctx.clone());
            }
            maybe_user = EUser::find_by_id(decoded.user_id())
                .one(&state.web_db)
                .await
                .ok()
                .flatten();
        }
    }

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
        .context("Failed to update user last login")
}
