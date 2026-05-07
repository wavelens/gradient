/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use gradient_core::types::*;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
use std::sync::Arc;

use super::jwt::{decode_jwt, extract_bearer_or_cookie, token_from_cookie};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::error::{WebError, WebResult};

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
    let info = RequestInfo::from_headers(req.headers());
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();

    let auth_header = req.headers().get(axum::http::header::AUTHORIZATION).cloned();

    let token_str: String = if let Some(header) = auth_header {
        let val = match header.to_str() {
            Ok(v) => v.to_string(),
            Err(_) => {
                audit_deny(&state, None, info, method, path, "Authorization header empty").await;
                return Err(WebError::forbidden("Authorization header empty"));
            }
        };
        let mut parts = val.split_whitespace();
        let (bearer, token) = (parts.next(), parts.next());
        if bearer != Some("Bearer") || token.is_none() {
            audit_deny(&state, None, info, method, path, "Invalid Authorization header").await;
            return Err(WebError::forbidden("Invalid Authorization header"));
        }
        token.unwrap().to_string()
    } else if let Some(t) = token_from_cookie(&req) {
        t
    } else {
        audit_deny(&state, None, info, method, path, "Authorization header not found").await;
        return Err(WebError::forbidden("Authorization header not found"));
    };

    let token_data = match decode_jwt(state.clone(), token_str).await {
        Ok(t) => t,
        Err(_) => {
            audit_deny(&state, None, info, method, path, "Unable to decode token").await;
            return Err(WebError::unauthorized("Unable to decode token"));
        }
    };

    let current_user = match EUser::find_by_id(token_data.claims.id).one(&state.web_db).await? {
        Some(u) => u,
        None => {
            audit_deny(
                &state,
                Some(token_data.claims.id),
                info,
                method,
                path,
                "User not found",
            )
            .await;
            return Err(WebError::unauthorized("User not found"));
        }
    };

    req.extensions_mut().insert(current_user);
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
    let maybe_user = if let Some(token_str) = extract_bearer_or_cookie(req.headers()) {
        if let Ok(token_data) = decode_jwt(State(Arc::clone(&state)), token_str).await {
            EUser::find_by_id(token_data.claims.id)
                .one(&state.web_db)
                .await
                .ok()
                .flatten()
        } else {
            None
        }
    } else {
        None
    };
    req.extensions_mut().insert(MaybeUser(maybe_user));
    next.run(req).await
}

pub async fn update_last_login(state: State<Arc<ServerState>>, user: MUser) -> Result<MUser> {
    let mut auser: AUser = user.into();

    auser.last_login_at = Set(gradient_core::types::now());
    auser
        .update(&state.web_db)
        .await
        .context("Failed to update user last login")
}
