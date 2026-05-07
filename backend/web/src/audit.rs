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

use axum::http::HeaderMap;
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ConnectionTrait, EntityTrait};

/// Audit event identifiers. Stored as plain strings so adding new variants
/// never requires a DB migration.
pub mod events {
    pub const LOGIN_SUCCESS: &str = "login.success";
    pub const LOGIN_FAILURE: &str = "login.failure";
    pub const LOGOUT: &str = "logout";
    pub const REGISTER: &str = "register";
    pub const USER_DELETE: &str = "user.delete";
    pub const API_KEY_CREATE: &str = "api_key.create";
    pub const API_KEY_REVOKE: &str = "api_key.revoke";
    pub const API_KEY_DELETE: &str = "api_key.delete";
    pub const SESSION_REVOKE: &str = "session.revoke";
    pub const AUTH_DENY: &str = "auth.deny";
    pub const ORG_DELETE: &str = "organization.delete";
    pub const ORG_MEMBER_ADD: &str = "organization.member.add";
    pub const ORG_MEMBER_REMOVE: &str = "organization.member.remove";
    pub const ORG_MEMBER_ROLE_CHANGE: &str = "organization.member.role_change";
    pub const PROJECT_DELETE: &str = "project.delete";
    pub const CACHE_DELETE: &str = "cache.delete";
}

/// Caller context derived from the inbound HTTP request — used to enrich
/// audit log rows and `session` rows with IP and user-agent for the
/// "logged-in devices" UI.
#[derive(Debug, Clone, Default)]
pub struct RequestInfo {
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

impl RequestInfo {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let ip = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(',').next())
            .map(|s| s.trim().to_string())
            .or_else(|| {
                headers
                    .get("x-real-ip")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
            });
        let user_agent = headers
            .get(axum::http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        Self { ip, user_agent }
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
