/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use gradient_core::ServerState;
use gradient_db::lookup::by_id;
use gradient_types::events::audit::Action;
use gradient_types::events::{Event, EventRx};
use gradient_types::*;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{Instant, interval_at};

use super::jwt::{decode_jwt, extract_bearer_or_cookie};
use super::middleware::MaybeUser;

const LIVE_WEBSOCKET_SESSION_OR_API_KEY_RECHECK_INTERVAL: Duration = Duration::from_secs(60);

const AUDIT_ACTIONS_REQUIRING_WEBSOCKET_CREDENTIAL_RECHECK: &[Action] = &[
    Action::Logout,
    Action::SessionRevoke,
    Action::ApiKeyRevoke,
    Action::ApiKeyDelete,
    Action::UserDelete,
];

/// The socket can stay open only while the session or API key that opened it is valid. Rechecks occur on every matching audit event and at a fixed interval and that interval can also cover changes on other server instances and accounts that an admin disabled.
pub struct LiveStreamAuthToken(Option<SignedInCaller>);

struct SignedInCaller {
    state: Arc<ServerState>,
    token: String,
    user: UserId,
    audit_events: EventRx,
}

impl LiveStreamAuthToken {
    pub fn anonymous() -> Self {
        Self(None)
    }

    pub async fn wait_until_session_or_api_key_is_revoked(self) {
        let Some(mut caller) = self.0 else {
            return std::future::pending().await;
        };
        let period = LIVE_WEBSOCKET_SESSION_OR_API_KEY_RECHECK_INTERVAL;
        let mut recheck = interval_at(Instant::now() + period, period);
        loop {
            tokio::select! {
                _ = recheck.tick() => {}
                event = caller.audit_events.recv() => match event {
                    Ok(env) if !caller.ends_credentials(&env.event) => continue,
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return std::future::pending().await,
                },
            }
            if !caller.should_keep_stream_open().await {
                return;
            }
        }
    }
}

impl SignedInCaller {
    fn ends_credentials(&self, event: &Event) -> bool {
        matches!(event, Event::Audit(audited)
            if audited.user == Some(self.user)
                && AUDIT_ACTIONS_REQUIRING_WEBSOCKET_CREDENTIAL_RECHECK.contains(&audited.action))
    }

    async fn should_keep_stream_open(&self) -> bool {
        let state = &self.state;
        let decoded = decode_jwt(
            &state.web_db,
            &state.jwt_secret,
            &state.memory.last_used_stamps,
            self.token.clone(),
        )
        .await;
        match decoded {
            Ok(decoded) => match by_id::<EUser, _>(&state.web_db, decoded.user_id()).await {
                Ok(user) => user.is_some_and(|user| user.active),
                Err(_) => true,
            },
            Err(status) => status == StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl FromRequestParts<Arc<ServerState>> for LiveStreamAuthToken {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<ServerState>,
    ) -> Result<Self, Self::Rejection> {
        let user = parts
            .extensions
            .get::<MUser>()
            .or_else(|| parts.extensions.get::<MaybeUser>()?.0.as_ref())
            .map(|user| user.id);
        let token = extract_bearer_or_cookie(&parts.headers);
        Ok(Self(user.zip(token).map(|(user, token)| SignedInCaller {
            state: Arc::clone(state),
            token: token.to_owned(),
            user,
            audit_events: state.events.subscribe(),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::api;
    use gradient_fixtures::mock::mock_db;
    use gradient_test_support::state::TestState;
    use gradient_types::events::EventOwner;
    use gradient_types::events::audit::Audited;

    fn audited(action: Action, user: UserId) -> Audited {
        Audited {
            action,
            user: Some(user),
            owner: EventOwner::default(),
            metadata: None,
        }
    }

    fn api_key_caller(user: UserId) -> (Arc<ServerState>, LiveStreamAuthToken) {
        let db = mock_db()
            .append_query_results([Vec::<api::Model>::new()])
            .into_connection();
        let state = TestState::new().web_db(db).build();
        let caller = SignedInCaller {
            state: Arc::clone(&state),
            token: format!("GRAD{}", "k".repeat(64)),
            user,
            audit_events: state.events.subscribe(),
        };
        (state, LiveStreamAuthToken(Some(caller)))
    }

    #[tokio::test]
    async fn a_revoked_api_key_ends_the_stream_right_after_its_audit_event() {
        let user = UserId::now_v7();
        let (state, credentials) = api_key_caller(user);
        state.events.publish(audited(Action::ApiKeyRevoke, user));

        tokio::time::timeout(
            Duration::from_secs(5),
            credentials.wait_until_session_or_api_key_is_revoked(),
        )
        .await
        .expect("the stream ends once the key no longer decodes");
    }

    #[tokio::test]
    async fn only_credential_events_of_the_same_user_trigger_a_recheck() {
        let user = UserId::now_v7();
        let (_state, credentials) = api_key_caller(user);
        let caller = credentials.0.expect("signed in");

        assert!(caller.ends_credentials(&Event::Audit(audited(Action::SessionRevoke, user))));
        assert!(caller.ends_credentials(&Event::Audit(audited(Action::Logout, user))));
        let other = UserId::now_v7();
        assert!(!caller.ends_credentials(&Event::Audit(audited(Action::SessionRevoke, other))));
        assert!(!caller.ends_credentials(&Event::Audit(audited(Action::ProjectCreate, user))));
    }

    #[tokio::test]
    async fn an_anonymous_stream_never_ends_on_its_own() {
        let ended = tokio::time::timeout(
            Duration::from_millis(50),
            LiveStreamAuthToken::anonymous().wait_until_session_or_api_key_is_revoked(),
        )
        .await;
        assert!(ended.is_err());
    }
}
