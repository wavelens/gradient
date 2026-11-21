/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::ws::WebSocket;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::header::ORIGIN;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method};
use axum::response::{IntoResponse, Response};
use futures::future::BoxFuture;
use gradient_db::WebDb;
use gradient_db::lookup::by_id;
use gradient_types::events::EventRx;
use gradient_types::events::access_recheck::wait_until_access_check_fails;
use gradient_types::*;
use gradient_util::debounce::Debounce;
use std::future::Future;
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::OwnedSemaphorePermit;
use url::{Origin, Url};
use uuid::Uuid;

use super::api_key::{ApiKeyContext, MaybeApiKey};
use super::jwt::{bearer_token, decode_jwt, extract_bearer_or_cookie, session_cookie_token};
use super::middleware::MaybeUser;
use crate::client_ip::{ClientIp, rate_limit_key_for_client_ip};
use crate::error::{WebError, WebResult};
use crate::ip_allowlist::is_allowed as ip_allowed;
use crate::per_key_open_slot_limit::OpenSlotLimitPerKey;
use crate::refusal::{Refusal, RefusalStatus};

pub(crate) type AnonymousWebSocketOpenLimitByIpAddress = OpenSlotLimitPerKey<IpAddr>;

pub type WebsocketCredentialRecheckClosure = Arc<
    dyn Fn(Option<MUser>, Option<ApiKeyContext>) -> BoxFuture<'static, WebResult<()>> + Send + Sync,
>;

#[derive(Clone)]
pub struct WebSocketCredentialRecheckDependencies {
    pub(crate) web_db: WebDb,
    pub(crate) jwt_secret: SecretString,
    pub(crate) last_used_stamps: Arc<Debounce<Uuid>>,
    pub(crate) events: EventBus,
}

pub struct WebSocketUpgradeCredentialRecheckRequestExtractor {
    credential: OpenWebSocketCallerRecheckInputs,
    #[expect(
        clippy::disallowed_types,
        reason = "this extractor can check the Origin first and take the upgrade only then"
    )]
    upgrade: Result<axum::extract::ws::WebSocketUpgrade, WebSocketUpgradeRejection>,
}

struct OpenWebSocketCallerRecheckInputs {
    dependencies: WebSocketCredentialRecheckDependencies,
    token: Option<String>,
    client_ip: IpAddr,
    audit_events: EventRx,
    caller_user_id: Option<UserId>,
    anonymous_caller_websocket_slot_permit: Option<OwnedSemaphorePermit>,
}

/// WebSocket connections stay open only while callers hold access rights. Guards recheck credentials and the access rules of the handler every minute and after every audit event for access changes. All failures end the stream. Database errors count as failures.
pub struct WebSocketAuthorizationRecheck {
    audit_events: EventRx,
    caller_user_id: Option<UserId>,
    credential_and_handler_access_check:
        AlreadyOpenConnectionCredentialAndHandlerAccessRecheckInputs,
    anonymous_caller_websocket_slot_permit: Option<OwnedSemaphorePermit>,
}

struct AlreadyOpenConnectionCredentialAndHandlerAccessRecheckInputs {
    dependencies: WebSocketCredentialRecheckDependencies,
    token: Option<String>,
    client_ip: IpAddr,
    recheck: WebsocketCredentialRecheckClosure,
    superuser_permission_check_http_method: Method,
}

pub struct AccessCheckedWebSocketUpgrade {
    #[expect(
        clippy::disallowed_types,
        reason = "this extractor can check the Origin first and take the upgrade only then"
    )]
    upgrade: Result<axum::extract::ws::WebSocketUpgrade, WebSocketUpgradeRejection>,
    recheck: WebSocketAuthorizationRecheck,
}

impl WebSocketUpgradeCredentialRecheckRequestExtractor {
    pub fn watch_caller_access_with_recheck<F, Fut>(
        self,
        superuser_permission_check_http_method: Method,
        recheck: F,
    ) -> AccessCheckedWebSocketUpgrade
    where
        F: Fn(Option<MUser>, Option<ApiKeyContext>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = WebResult<()>> + Send + 'static,
    {
        AccessCheckedWebSocketUpgrade {
            upgrade: self.upgrade,
            recheck: self
                .credential
                .watch_caller_access_with_recheck(superuser_permission_check_http_method, recheck),
        }
    }
}

impl OpenWebSocketCallerRecheckInputs {
    fn watch_caller_access_with_recheck<F, Fut>(
        self,
        superuser_permission_check_http_method: Method,
        recheck: F,
    ) -> WebSocketAuthorizationRecheck
    where
        F: Fn(Option<MUser>, Option<ApiKeyContext>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = WebResult<()>> + Send + 'static,
    {
        WebSocketAuthorizationRecheck {
            audit_events: self.audit_events,
            caller_user_id: self.caller_user_id,
            credential_and_handler_access_check:
                AlreadyOpenConnectionCredentialAndHandlerAccessRecheckInputs {
                    dependencies: self.dependencies,
                    token: self.token,
                    client_ip: self.client_ip,
                    recheck: Arc::new(move |user, api_key| Box::pin(recheck(user, api_key))),
                    superuser_permission_check_http_method,
                },
            anonymous_caller_websocket_slot_permit: self.anonymous_caller_websocket_slot_permit,
        }
    }
}

impl AccessCheckedWebSocketUpgrade {
    pub fn set_max_message_and_frame_size_in_bytes(mut self, max_bytes: usize) -> Self {
        self.upgrade = self.upgrade.map(|upgrade| {
            upgrade
                .max_message_size(max_bytes)
                .max_frame_size(max_bytes)
        });
        self
    }

    pub fn on_upgrade<C, Fut>(self, callback: C) -> Response
    where
        C: FnOnce(WebSocket, WebSocketAuthorizationRecheck) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        match self.upgrade {
            Ok(upgrade) => {
                let recheck = self.recheck;
                upgrade.on_upgrade(move |socket| callback(socket, recheck))
            }
            Err(rejection) => rejection.into_response(),
        }
    }
}

impl WebSocketAuthorizationRecheck {
    pub fn new_for_non_websocket_caller<F, Fut>(
        dependencies: WebSocketCredentialRecheckDependencies,
        token: String,
        client_ip: IpAddr,
        caller_user_id: UserId,
        superuser_permission_check_http_method: Method,
        recheck: F,
    ) -> Self
    where
        F: Fn(Option<MUser>, Option<ApiKeyContext>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = WebResult<()>> + Send + 'static,
    {
        OpenWebSocketCallerRecheckInputs {
            audit_events: dependencies.events.subscribe(),
            dependencies,
            token: Some(token),
            client_ip,
            caller_user_id: Some(caller_user_id),
            anonymous_caller_websocket_slot_permit: None,
        }
        .watch_caller_access_with_recheck(superuser_permission_check_http_method, recheck)
    }

    pub async fn wait_until_access_lost(self) {
        let _anonymous_caller_websocket_slot_permit = self.anonymous_caller_websocket_slot_permit;
        let credential_access_recheck_inputs = &self.credential_and_handler_access_check;
        wait_until_access_check_fails(self.audit_events, self.caller_user_id, move || {
            credential_access_recheck_inputs.is_credential_and_access_still_allowed()
        })
        .await;
    }
}

impl AlreadyOpenConnectionCredentialAndHandlerAccessRecheckInputs {
    async fn is_credential_and_access_still_allowed(&self) -> bool {
        let Some(token) = &self.token else {
            return (self.recheck)(None, None).await.is_ok();
        };
        let dependencies = &self.dependencies;
        let Ok(decoded) = decode_jwt(
            &dependencies.web_db,
            &dependencies.jwt_secret,
            &dependencies.last_used_stamps,
            token.clone(),
        )
        .await
        else {
            return false;
        };
        let api_key = decoded.api_key_context().cloned();
        if api_key
            .as_ref()
            .is_some_and(|key| !ip_allowed(self.client_ip, &key.allowed_ips))
        {
            return false;
        }
        let Ok(Some(user)) = by_id::<EUser, _>(&dependencies.web_db, decoded.user_id()).await
        else {
            return false;
        };
        if !user.active {
            return false;
        }
        let user = decoded.user_with_superuser_allowed_for_credential(
            user,
            &self.superuser_permission_check_http_method,
        );
        (self.recheck)(Some(user), api_key).await.is_ok()
    }
}

impl<S> FromRequestParts<S> for WebSocketUpgradeCredentialRecheckRequestExtractor
where
    S: Send + Sync,
    WebSocketCredentialRecheckDependencies: FromRef<S>,
{
    type Rejection = WebError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let ClientIp(client_ip) = *parts
            .extensions
            .get::<ClientIp>()
            .ok_or_else(|| anyhow::anyhow!("client IP missing on a websocket route"))?;
        let own_origin = parts
            .extensions
            .get::<ServeUrlOrigin>()
            .ok_or_else(|| anyhow::anyhow!("serve_url origin missing on a websocket route"))?;
        if is_session_cookie_request_without_bearer_token_from_other_site_origin(
            &parts.headers,
            own_origin,
        ) {
            return Err(ForeignOriginCookieWebsocketUpgradeRefusal::ForeignOrigin.into());
        }
        let dependencies = WebSocketCredentialRecheckDependencies::from_ref(state);
        #[expect(
            clippy::disallowed_types,
            reason = "The upgrade can only happen after the Origin check above."
        )]
        let upgrade = axum::extract::ws::WebSocketUpgrade::from_request_parts(parts, state).await;
        let caller_user_id = parts
            .extensions
            .get::<MUser>()
            .or_else(|| parts.extensions.get::<MaybeUser>()?.0.as_ref())
            .map(|user| user.id);
        let has_api_key = parts
            .extensions
            .get::<MaybeApiKey>()
            .is_some_and(|api_key| api_key.0.is_some());
        let anonymous_caller_websocket_slot_permit = if caller_user_id.is_none() && !has_api_key {
            let limit = parts
                .extensions
                .get::<Arc<AnonymousWebSocketOpenLimitByIpAddress>>()
                .ok_or_else(|| {
                    anyhow::anyhow!("open anonymous websocket limit missing on a websocket route")
                })?;
            Some(
                limit
                    .try_acquire_open_slot_for_key(rate_limit_key_for_client_ip(client_ip))
                    .ok_or(AnonymousWebsocketConnectionLimitReachedRefusal::ClientAddressAtAnonymousWebSocketLimit)?,
            )
        } else {
            None
        };
        Ok(Self {
            credential: OpenWebSocketCallerRecheckInputs {
                audit_events: dependencies.events.subscribe(),
                token: extract_bearer_or_cookie(&parts.headers).map(str::to_owned),
                client_ip,
                dependencies,
                caller_user_id,
                anonymous_caller_websocket_slot_permit,
            },
            upgrade,
        })
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum ForeignOriginCookieWebsocketUpgradeRefusal {
    #[error(
        "Allow WebSocket upgrades that use cookies for sign-in only from the origin of this server itself."
    )]
    ForeignOrigin,
}

impl Refusal for ForeignOriginCookieWebsocketUpgradeRefusal {
    fn status(&self) -> RefusalStatus {
        RefusalStatus::Forbidden
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum AnonymousWebsocketConnectionLimitReachedRefusal {
    #[error(
        "Too many live connections from this network address. Close the pages with live updates or sign in, then try again."
    )]
    ClientAddressAtAnonymousWebSocketLimit,
}

impl Refusal for AnonymousWebsocketConnectionLimitReachedRefusal {
    fn status(&self) -> RefusalStatus {
        RefusalStatus::TooManyRequests
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServeUrlOrigin(Origin);

impl ServeUrlOrigin {
    pub fn parse(serve_url: &str) -> Option<Self> {
        let origin = Url::parse(serve_url).ok()?.origin();
        origin.is_tuple().then_some(Self(origin))
    }

    pub fn ascii_serialization(&self) -> String {
        self.0.ascii_serialization()
    }

    fn origin_header_matches_server_origin(&self, origin_header: &HeaderValue) -> bool {
        origin_header
            .to_str()
            .ok()
            .and_then(|origin| Url::parse(origin).ok())
            .is_some_and(|origin| origin.origin() == self.0)
    }
}

fn is_session_cookie_request_without_bearer_token_from_other_site_origin(
    headers: &HeaderMap,
    own_origin: &ServeUrlOrigin,
) -> bool {
    let signs_in_with_cookie_only =
        bearer_token(headers).is_none() && session_cookie_token(headers).is_some();
    signs_in_with_cookie_only
        && headers
            .get(ORIGIN)
            .is_some_and(|origin| !own_origin.origin_header_matches_server_origin(origin))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::{AUTHORIZATION, COOKIE};
    use gradient_entity::api;
    use gradient_fixtures::mock::mock_db;
    use gradient_test_support::state::TestState;
    use gradient_types::events::EventOwner;
    use gradient_types::events::audit::{Action, Audited};
    use sea_orm::{DbErr, MockDatabase};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn audited(action: Action, user: UserId) -> Audited {
        Audited {
            action,
            user: Some(user),
            owner: EventOwner::default(),
            metadata: None,
        }
    }

    fn credential(
        db: MockDatabase,
        token: Option<String>,
        caller_user_id: Option<UserId>,
    ) -> (EventBus, OpenWebSocketCallerRecheckInputs) {
        let state = TestState::new().web_db(db.into_connection()).build();
        let dependencies = WebSocketCredentialRecheckDependencies::from_ref(&state);
        let events = dependencies.events.clone();
        let credential = OpenWebSocketCallerRecheckInputs {
            audit_events: events.subscribe(),
            dependencies,
            token,
            client_ip: IpAddr::from([127, 0, 0, 1]),
            caller_user_id,
            anonymous_caller_websocket_slot_permit: None,
        };
        (events, credential)
    }

    fn api_key_token() -> Option<String> {
        Some(format!("GRAD{}", "k".repeat(64)))
    }

    #[tokio::test]
    async fn a_revoked_api_key_ends_the_stream_right_after_its_audit_event() {
        let key_owner = UserId::now_v7();
        let db = mock_db().append_query_results([Vec::<api::Model>::new()]);
        let (events, credential) = credential(db, api_key_token(), Some(key_owner));
        let guard =
            credential.watch_caller_access_with_recheck(Method::GET, |_, _| async { Ok(()) });
        events.publish(audited(Action::ApiKeyRevoke, key_owner));

        tokio::time::timeout(Duration::from_secs(5), guard.wait_until_access_lost())
            .await
            .expect("the stream ends once the key no longer decodes");
    }

    #[tokio::test]
    async fn a_database_error_during_the_recheck_ends_the_stream() {
        let session_owner = UserId::now_v7();
        let db = mock_db().append_query_errors([DbErr::Custom("database unreachable".into())]);
        let (events, credential) = credential(db, api_key_token(), Some(session_owner));
        let guard =
            credential.watch_caller_access_with_recheck(Method::GET, |_, _| async { Ok(()) });
        events.publish(audited(Action::SessionRevoke, session_owner));

        tokio::time::timeout(Duration::from_secs(5), guard.wait_until_access_lost())
            .await
            .expect("a failed recheck must close the socket");
    }

    #[tokio::test]
    async fn a_failing_access_check_ends_an_anonymous_stream_after_a_project_update() {
        let (events, credential) = credential(mock_db(), None, None);
        let guard = credential.watch_caller_access_with_recheck(Method::GET, |_, _| async {
            Err(WebError::not_found(crate::resource::Resource::Project))
        });
        events.publish(audited(Action::ProjectUpdate, UserId::now_v7()));

        tokio::time::timeout(Duration::from_secs(5), guard.wait_until_access_lost())
            .await
            .expect("a project that turned private ends the stream");
    }

    #[tokio::test(start_paused = true)]
    async fn the_access_check_reruns_every_minute_without_events() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let (_events, credential) = credential(mock_db(), None, None);
        let guard = credential.watch_caller_access_with_recheck(Method::GET, move |_, _| {
            let calls = Arc::clone(&counted);
            async move {
                match calls.fetch_add(1, Ordering::SeqCst) {
                    0 => Ok(()),
                    _ => Err(WebError::not_found(crate::resource::Resource::Project)),
                }
            }
        });

        tokio::time::timeout(Duration::from_secs(150), guard.wait_until_access_lost())
            .await
            .expect("the second periodic recheck fails and ends the stream");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn events_that_do_not_change_access_keep_the_stream_open() {
        let (events, credential) = credential(mock_db(), None, None);
        let guard = credential.watch_caller_access_with_recheck(Method::GET, |_, _| async {
            Err(WebError::not_found(crate::resource::Resource::Project))
        });
        events.publish(audited(Action::ProjectStar, UserId::now_v7()));

        let ended =
            tokio::time::timeout(Duration::from_millis(50), guard.wait_until_access_lost()).await;
        assert!(ended.is_err());
    }

    async fn build_denied_access_recheck_guard_and_audit_event_bus_with_optional_signed_in_user_and_call_counter(
        signed_in_user: Option<MUser>,
    ) -> (EventBus, Arc<AtomicUsize>, WebSocketAuthorizationRecheck) {
        let state = TestState::new().web_db(mock_db().into_connection()).build();
        let mut request = axum::http::Request::builder()
            .extension(ClientIp(IpAddr::from([127, 0, 0, 1])))
            .extension(ServeUrlOrigin::parse("https://ci.example.com").unwrap())
            .extension(Arc::new(AnonymousWebSocketOpenLimitByIpAddress::new(1)));
        request = match signed_in_user {
            Some(user) => request.extension(user),
            None => request.extension(MaybeUser(None)),
        };
        let (mut parts, ()) = request.body(()).unwrap().into_parts();
        let extractor = WebSocketUpgradeCredentialRecheckRequestExtractor::from_request_parts(
            &mut parts, &state,
        )
        .await
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let upgrade = extractor.watch_caller_access_with_recheck(Method::GET, move |_, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            async { Err(WebError::not_found(crate::resource::Resource::Project)) }
        });
        let events = WebSocketCredentialRecheckDependencies::from_ref(&state).events;
        (events, calls, upgrade.recheck)
    }

    #[tokio::test]
    async fn audit_event_on_user_account_does_not_recheck_anonymous_websocket_stream() {
        let (events, calls, guard) =
            build_denied_access_recheck_guard_and_audit_event_bus_with_optional_signed_in_user_and_call_counter(None)
                .await;
        events.publish(audited(Action::ApiKeyRevoke, UserId::now_v7()));

        let ended =
            tokio::time::timeout(Duration::from_millis(50), guard.wait_until_access_lost()).await;
        assert!(ended.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn audit_event_on_other_user_account_does_not_recheck_signed_in_stream_but_own_account_ends_it()
     {
        let stream_user = gradient_fixtures::rows::user();
        let (events, calls, guard) =
            build_denied_access_recheck_guard_and_audit_event_bus_with_optional_signed_in_user_and_call_counter(
                Some(stream_user.clone()),
            )
            .await;
        events.publish(audited(Action::SessionRevoke, UserId::now_v7()));

        let open_after_foreign_event =
            tokio::time::timeout(Duration::from_millis(50), guard.wait_until_access_lost());
        assert!(open_after_foreign_event.await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let (events, _, guard) =
            build_denied_access_recheck_guard_and_audit_event_bus_with_optional_signed_in_user_and_call_counter(
                Some(stream_user.clone()),
            )
            .await;
        events.publish(audited(Action::SessionRevoke, stream_user.id));
        tokio::time::timeout(Duration::from_secs(5), guard.wait_until_access_lost())
            .await
            .expect("an event on the account of the stream user must trigger a recheck and end the stream");
    }

    #[tokio::test]
    async fn several_queued_access_change_audit_events_cause_one_access_check() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let (events, credential) = credential(mock_db(), None, None);
        let guard = credential.watch_caller_access_with_recheck(Method::GET, move |_, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        });
        for _ in 0..5 {
            events.publish(audited(Action::ProjectUpdate, UserId::now_v7()));
        }

        let ended =
            tokio::time::timeout(Duration::from_millis(50), guard.wait_until_access_lost()).await;
        assert!(ended.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    async fn run_websocket_upgrade_extractor_with_client_ip_user_and_api_key(
        limit: &Arc<AnonymousWebSocketOpenLimitByIpAddress>,
        client_ip: &str,
        signed_in_user: Option<MUser>,
        api_key: Option<ApiKeyContext>,
    ) -> Result<WebSocketUpgradeCredentialRecheckRequestExtractor, WebError> {
        let state = TestState::new().web_db(mock_db().into_connection()).build();
        let request = axum::http::Request::builder()
            .extension(ClientIp(client_ip.parse().expect("client ip")))
            .extension(ServeUrlOrigin::parse("https://ci.example.com").unwrap())
            .extension(Arc::clone(limit))
            .extension(MaybeUser(signed_in_user))
            .extension(MaybeApiKey(api_key));
        let (mut parts, ()) = request.body(()).unwrap().into_parts();
        WebSocketUpgradeCredentialRecheckRequestExtractor::from_request_parts(&mut parts, &state)
            .await
    }

    fn assert_refused_with_too_many_requests(
        upgrade: Result<WebSocketUpgradeCredentialRecheckRequestExtractor, WebError>,
    ) {
        let refusal = upgrade.err().expect("upgrade over the cap is refused");
        assert_eq!(refusal.status(), axum::http::StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn two_anonymous_websocket_upgrades_accepted_third_refused_with_429() {
        let limit = Arc::new(AnonymousWebSocketOpenLimitByIpAddress::new(2));
        let mut open = Vec::new();
        for _ in 0..2 {
            open.push(
                run_websocket_upgrade_extractor_with_client_ip_user_and_api_key(
                    &limit,
                    "192.0.2.1",
                    None,
                    None,
                )
                .await
                .expect("upgrade under the cap"),
            );
        }
        assert_refused_with_too_many_requests(
            run_websocket_upgrade_extractor_with_client_ip_user_and_api_key(
                &limit,
                "192.0.2.1",
                None,
                None,
            )
            .await,
        );
    }

    #[tokio::test]
    async fn anonymous_upgrade_from_same_address_is_accepted_after_guard_drop_frees_slot() {
        let limit = Arc::new(AnonymousWebSocketOpenLimitByIpAddress::new(1));
        let open = run_websocket_upgrade_extractor_with_client_ip_user_and_api_key(
            &limit,
            "192.0.2.1",
            None,
            None,
        )
        .await
        .expect("upgrade under the cap")
        .watch_caller_access_with_recheck(Method::GET, |_, _| async { Ok(()) });
        assert_refused_with_too_many_requests(
            run_websocket_upgrade_extractor_with_client_ip_user_and_api_key(
                &limit,
                "192.0.2.1",
                None,
                None,
            )
            .await,
        );
        drop(open);
        run_websocket_upgrade_extractor_with_client_ip_user_and_api_key(
            &limit,
            "192.0.2.1",
            None,
            None,
        )
        .await
        .expect("the slot of the ended socket is free again");
    }

    #[tokio::test]
    async fn signed_in_user_and_api_key_caller_upgrade_when_anonymous_connection_cap_is_reached() {
        let limit = Arc::new(AnonymousWebSocketOpenLimitByIpAddress::new(1));
        let _anonymous = run_websocket_upgrade_extractor_with_client_ip_user_and_api_key(
            &limit,
            "192.0.2.1",
            None,
            None,
        )
        .await
        .expect("upgrade under the cap");
        let api_key = ApiKeyContext {
            mask: 0,
            cache_permission_mask: 0,
            project: None,
            cache_pin: None,
            allowed_ips: Vec::new(),
            job_binding: None,
        };
        let mut open = Vec::new();
        for _ in 0..2 {
            open.push(
                run_websocket_upgrade_extractor_with_client_ip_user_and_api_key(
                    &limit,
                    "192.0.2.1",
                    Some(gradient_fixtures::rows::user()),
                    None,
                )
                .await
                .expect("a signed-in caller is not counted"),
            );
            open.push(
                run_websocket_upgrade_extractor_with_client_ip_user_and_api_key(
                    &limit,
                    "192.0.2.1",
                    None,
                    Some(api_key.clone()),
                )
                .await
                .expect("an API key caller is not counted"),
            );
        }
    }

    #[tokio::test]
    async fn anonymous_websocket_limits_are_separate_for_ipv4_addresses_and_shared_for_addresses_in_one_ipv6_64_subnet()
     {
        let limit = Arc::new(AnonymousWebSocketOpenLimitByIpAddress::new(1));
        let mut open = Vec::new();
        for client_ip in ["192.0.2.1", "192.0.2.2", "2001:db8:0:1::1"] {
            open.push(
                run_websocket_upgrade_extractor_with_client_ip_user_and_api_key(
                    &limit, client_ip, None, None,
                )
                .await
                .expect("each address has its own slot"),
            );
        }
        assert_refused_with_too_many_requests(
            run_websocket_upgrade_extractor_with_client_ip_user_and_api_key(
                &limit,
                "2001:db8:0:1:ffff::2",
                None,
                None,
            )
            .await,
        );
    }

    fn request_headers_with_session_cookie_and_origin(origin: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_static("jwt_token=abc"));
        headers.insert(ORIGIN, HeaderValue::from_static(origin));
        headers
    }

    #[test]
    fn every_serve_url_spelling_accepts_server_origin() {
        for serve_url in [
            "https://ci.example.com",
            "https://ci.example.com/",
            "https://ci.example.com/gradient/",
            "https://CI.Example.com",
            "https://ci.example.com:443",
        ] {
            let own = ServeUrlOrigin::parse(serve_url).expect(serve_url);
            let headers = request_headers_with_session_cookie_and_origin("https://ci.example.com");
            assert!(
                !is_session_cookie_request_without_bearer_token_from_other_site_origin(
                    &headers, &own
                ),
                "{serve_url}"
            );
        }
    }

    #[test]
    fn websocket_cookie_request_with_different_or_invalid_origin_is_refused() {
        let own = ServeUrlOrigin::parse("https://ci.example.com").unwrap();
        for origin in [
            "https://evil.example",
            "http://ci.example.com",
            "https://ci.example.com:8443",
            "null",
            "not an origin",
        ] {
            let headers = request_headers_with_session_cookie_and_origin(origin);
            assert!(
                is_session_cookie_request_without_bearer_token_from_other_site_origin(
                    &headers, &own
                ),
                "{origin}"
            );
        }
    }

    #[test]
    fn only_bearer_authorization_header_skips_origin_check_of_cookie_request() {
        let own = ServeUrlOrigin::parse("https://ci.example.com").unwrap();
        let mut headers = request_headers_with_session_cookie_and_origin("https://evil.example");
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Basic dXNlcjpwYXNz"),
        );
        assert!(
            is_session_cookie_request_without_bearer_token_from_other_site_origin(&headers, &own)
        );

        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer GRADx"));
        assert!(
            !is_session_cookie_request_without_bearer_token_from_other_site_origin(&headers, &own)
        );

        headers.remove(AUTHORIZATION);
        headers.remove(COOKIE);
        assert!(
            !is_session_cookie_request_without_bearer_token_from_other_site_origin(&headers, &own)
        );
    }

    #[test]
    fn serve_url_without_scheme_and_host_or_file_url_returns_no_server_origin() {
        assert_eq!(ServeUrlOrigin::parse("ci.example.com"), None);
        assert_eq!(ServeUrlOrigin::parse("file:///srv/gradient"), None);
    }
}
