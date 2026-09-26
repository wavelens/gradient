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
use gradient_core::ServerState;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::{Action, Audited};
use gradient_types::*;
use sea_orm::{EntityTrait, IntoActiveModel};
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

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

/// Insert an `audit_log` row, emit a structured tracing event and record the
/// durable `Audited` event. DB errors are warned and dropped; the tracing event
/// always fires so operators tailing the live log see security-relevant
/// activity even if the DB insert is failing.
pub async fn record(
    state: &ServerState,
    user_id: Option<UserId>,
    action: Action,
    owner: EventOwner,
    info: &RequestInfo,
    metadata: Option<serde_json::Value>,
) {
    let event = action.name();
    tracing::info!(
        target: "audit",
        event,
        user_id = user_id.map(|id| id.to_string()),
        ip = info.ip.as_deref(),
        user_agent = info.user_agent.as_deref(),
        metadata = metadata.as_ref().map(|m| m.to_string()),
        "security event",
    );

    let row = MAuditLog {
        id: AuditLogId::now_v7(),
        user_id,
        event: event.to_owned(),
        ip: info.ip.clone(),
        user_agent: info.user_agent.clone(),
        metadata: metadata.clone(),
        created_at: gradient_types::now(),
    }
    .into_active_model();

    if let Err(e) = EAuditLog::insert(row).exec(&state.web_db).await {
        tracing::warn!(event, error = %e, "failed to write audit_log entry");
    }

    state
        .record(Audited {
            action,
            user: user_id,
            owner,
            metadata,
        })
        .await;
}

/// Audit metadata `{"fields": [...]}` naming the fields a patch request set.
pub fn changed_fields<const N: usize>(fields: [(&str, bool); N]) -> serde_json::Value {
    let set: Vec<&str> = fields
        .into_iter()
        .filter_map(|(name, set)| set.then_some(name))
        .collect();
    serde_json::json!({ "fields": set })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::Event;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    #[tokio::test]
    async fn audit_record_publishes_the_typed_event_with_its_owner() {
        let worker_db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let state = gradient_test_support::state::test_state(worker_db);
        let mut rx = state.events.subscribe();
        let user = UserId::now_v7();
        let owner = EventOwner {
            project: Some(ProjectId::now_v7()),
            ..Default::default()
        };

        record(
            &state,
            Some(user),
            Action::ProjectDelete,
            owner,
            &RequestInfo::default(),
            None,
        )
        .await;

        let envelope = rx.try_recv().expect("audit event published");
        let Event::Audit(audited) = &envelope.event else {
            panic!("expected an audit event, got {}", envelope.event.name());
        };
        assert_eq!(audited.action, Action::ProjectDelete);
        assert_eq!(audited.user, Some(user));
        assert_eq!(audited.owner, owner);
    }
}
