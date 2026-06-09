/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Append-only audit log writer for security-relevant events.
//!
//! Failures are intentionally swallowed (warn-only): an audit insert error
//! must never fail the underlying operation it is recording. Operators
//! monitor the warning log for missing audit rows; user-facing endpoints
//! never see a 5xx because of an audit write.

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::HeaderMap;
use axum::http::request::Parts;
use gradient_core::types::*;
use gradient_core::ServerState;
use sea_orm::ActiveValue::Set;
use sea_orm::{ConnectionTrait, EntityTrait};
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

/// Audit event identifiers. Stored as plain strings so adding new variants
/// never requires a DB migration.
pub mod events {
    pub const LOGIN_SUCCESS: &str = "login.success";
    pub const LOGIN_FAILURE: &str = "login.failure";
    pub const LOGOUT: &str = "logout";
    pub const REGISTER: &str = "register";
    pub const USER_DELETE: &str = "user.delete";
    pub const API_KEY_CREATE: &str = "api_key.create";
    pub const API_KEY_UPDATE: &str = "api_key.update";
    pub const API_KEY_REVOKE: &str = "api_key.revoke";
    pub const API_KEY_DELETE: &str = "api_key.delete";
    pub const SESSION_REVOKE: &str = "session.revoke";
    pub const AUTH_DENY: &str = "auth.deny";
    pub const CLI_DEVICE_START: &str = "cli.device.start";
    pub const CLI_DEVICE_AUTHORIZE: &str = "cli.device.authorize";
    pub const CLI_DEVICE_DENY: &str = "cli.device.deny";
    pub const ORG_DELETE: &str = "organization.delete";
    pub const ORG_MEMBER_ADD: &str = "organization.member.add";
    pub const ORG_MEMBER_REMOVE: &str = "organization.member.remove";
    pub const ORG_MEMBER_ROLE_CHANGE: &str = "organization.member.role_change";
    pub const ORG_ROLE_CREATE: &str = "organization.role.create";
    pub const ORG_ROLE_UPDATE: &str = "organization.role.update";
    pub const ORG_ROLE_DELETE: &str = "organization.role.delete";
    pub const PROJECT_DELETE: &str = "project.delete";
    pub const CACHE_DELETE: &str = "cache.delete";
    pub const CACHE_NAR_DELETE: &str = "cache.nar.delete";
    pub const CACHE_NAR_UPLOAD: &str = "cache.nar.upload";
    pub const CACHE_ROLE_CREATE: &str = "cache.role.create";
    pub const CACHE_ROLE_UPDATE: &str = "cache.role.update";
    pub const CACHE_ROLE_DELETE: &str = "cache.role.delete";
    pub const CACHE_MEMBER_CREATE: &str = "cache.member.create";
    pub const CACHE_MEMBER_UPDATE: &str = "cache.member.update";
    pub const CACHE_MEMBER_DELETE: &str = "cache.member.delete";
}

/// Caller context derived from the inbound HTTP request - used to enrich
/// audit log rows and `session` rows with IP and user-agent for the
/// "logged-in devices" UI.
#[derive(Debug, Clone, Default)]
pub struct RequestInfo {
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

impl RequestInfo {
    pub fn from_request(
        headers: &HeaderMap,
        peer: IpAddr,
        trusted_proxies: &[ipnet::IpNet],
    ) -> Self {
        let ip =
            Some(crate::client_ip::resolve_client_ip(headers, peer, trusted_proxies).to_string());
        let user_agent = headers
            .get(axum::http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        Self { ip, user_agent }
    }
}

/// Axum extractor: resolves the caller's IP from `ConnectInfo + X-Forwarded-For`
/// against the configured trusted-proxy CIDR set. Falls back to `0.0.0.0` when
/// the runtime has no peer socket (e.g. `axum_test::TestServer` without
/// `into_make_service_with_connect_info`), mirroring the auth middleware so a
/// missing `ConnectInfo` never turns a 4xx into a 500.
impl FromRequestParts<Arc<ServerState>> for RequestInfo {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<ServerState>,
    ) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip())
            .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        Ok(Self::from_request(
            &parts.headers,
            peer,
            &state.config.network.trusted_proxies,
        ))
    }
}

/// Insert an `audit_log` row and emit a structured tracing event. DB errors
/// are warned and dropped; the tracing event always fires so operators
/// tailing the live log see security-relevant activity even if the DB
/// insert is failing.
pub async fn record<C: ConnectionTrait>(
    db: &C,
    user_id: Option<UserId>,
    event: &str,
    info: &RequestInfo,
    metadata: Option<serde_json::Value>,
) {
    tracing::info!(
        target: "audit",
        event,
        user_id = user_id.map(|id| id.to_string()),
        ip = info.ip.as_deref(),
        user_agent = info.user_agent.as_deref(),
        metadata = metadata.as_ref().map(|m| m.to_string()),
        "security event",
    );

    let row = AAuditLog {
        id: Set(AuditLogId::now_v7()),
        user_id: Set(user_id),
        event: Set(event.to_string()),
        ip: Set(info.ip.clone()),
        user_agent: Set(info.user_agent.clone()),
        metadata: Set(metadata),
        created_at: Set(gradient_core::types::now()),
    };
    if let Err(e) = EAuditLog::insert(row).exec(db).await {
        tracing::warn!(event, error = %e, "failed to write audit_log entry");
    }
}
