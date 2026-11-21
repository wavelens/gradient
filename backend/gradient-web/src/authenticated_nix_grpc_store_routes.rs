/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{
    RateLimitBucketOwner, WebSocketAuthorizationRecheck, WebSocketCredentialRecheckDependencies,
    bearer_token, decode_jwt, reject_job_token_without_project_credentials_or_privileged_actions,
};
use crate::client_ip::resolve_client_ip;
use crate::error::WebError;
use crate::ip_allowlist::is_allowed as ip_allowed;
use crate::per_key_open_slot_limit::OpenSlotLimitPerKey;
use crate::refusal::SharedRefusal;
use axum::Router;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, StatusCode};
use governor::middleware::NoOpMiddleware;
use gradient_daemon::grpc_store_server::{
    AuthenticatedGrpcCallWithOpenCallSlotGuard, AuthenticatedNixGrpcStoreService,
    GrpcCallCredentialChecker,
};
use gradient_ssh::{
    SshBackend, SshHandles, check_session_access_still_matches_api_key,
    open_project_store_backend_for_api_key,
};
use ipnet::IpNet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::sync::OwnedSemaphorePermit;
use tonic::{Extensions, Status};
use tower_governor::governor::GovernorConfig;
use tower_governor::key_extractor::KeyExtractor;
use tower_governor::{GovernorError, GovernorLayer};

const MAX_CONCURRENT_GRPC_STORE_CALLS_PER_API_KEY: usize = 32;

pub(crate) type OpenGrpcStoreCallLimitByCredential = OpenSlotLimitPerKey<RateLimitBucketOwner>;

impl Default for OpenGrpcStoreCallLimitByCredential {
    fn default() -> Self {
        Self::new(MAX_CONCURRENT_GRPC_STORE_CALLS_PER_API_KEY)
    }
}

impl OpenGrpcStoreCallLimitByCredential {
    fn take_open_call_slot_for_credential_or_fail_with_resource_exhausted(
        &self,
        credential: RateLimitBucketOwner,
    ) -> Result<OwnedSemaphorePermit, Status> {
        self.try_acquire_open_slot_for_key(credential).ok_or_else(|| {
            Status::resource_exhausted(
                "This API key already reached the maximum number of open calls to the gRPC store. Wait for one call to end.",
            )
        })
    }
}

pub struct GrpcCallerAuthenticator {
    pub(crate) credential_recheck_dependencies: WebSocketCredentialRecheckDependencies,
    pub(crate) trusted_proxies: Vec<IpNet>,
    pub(crate) handles: SshHandles,
    pub(crate) concurrent_open_grpc_call_limiter_per_credential: OpenGrpcStoreCallLimitByCredential,
}

impl GrpcCallCredentialChecker for GrpcCallerAuthenticator {
    type Backend = SshBackend;
    type OpenCallSlotPermit = OwnedSemaphorePermit;

    async fn check_call_credentials_and_return_store_backend(
        &self,
        headers: &HeaderMap,
        extensions: &Extensions,
    ) -> Result<AuthenticatedGrpcCallWithOpenCallSlotGuard<SshBackend, OwnedSemaphorePermit>, Status>
    {
        let token = bearer_token(headers).ok_or_else(|| {
            Status::unauthenticated(SharedRefusal::AuthorizationRequired.to_string())
        })?;
        let dependencies = &self.credential_recheck_dependencies;
        let decoded = decode_jwt(
            &dependencies.web_db,
            &dependencies.jwt_secret,
            &dependencies.last_used_stamps,
            token.to_owned(),
        )
        .await
        .map_err(|status| match status {
            StatusCode::UNAUTHORIZED => {
                Status::unauthenticated(SharedRefusal::InvalidToken.to_string())
            }
            _ => Status::internal("API key lookup failed"),
        })?;
        let (context, project) = decoded
            .api_key_context()
            .and_then(|context| Some((context, context.project?)))
            .ok_or_else(|| {
                Status::unauthenticated(
                    "Use an API key that is restricted to one project for access to the gRPC store.",
                )
            })?;
        reject_job_token_without_project_credentials_or_privileged_actions(&decoded)
            .map_err(|refusal| Status::permission_denied(refusal.to_string()))?;

        let peer = extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |info| info.0.ip());
        let client_ip = resolve_client_ip(headers, peer, &self.trusted_proxies);
        if !ip_allowed(client_ip, &context.allowed_ips) {
            return Err(Status::permission_denied(
                SharedRefusal::SourceIpNotAllowed.to_string(),
            ));
        }
        let open_call_slot_permit = self
            .concurrent_open_grpc_call_limiter_per_credential
            .take_open_call_slot_for_credential_or_fail_with_resource_exhausted(
                decoded.rate_limit_bucket(),
            )?;

        let project_nix_store_backend = open_project_store_backend_for_api_key(
            self.handles.clone(),
            decoded.user_id(),
            project,
            context.mask,
            context.cache_pin,
        )
        .await
        .map_err(|error| {
            tracing::warn!(error = %error, "Failed to open the project store");
            Status::internal("Failed to open the project store")
        })?
        .ok_or_else(|| Status::permission_denied(SharedRefusal::MissingPermission.to_string()))?;
        let project_store_backend_for_access_recheck = Arc::clone(&project_nix_store_backend);
        let open_grpc_store_call_access_recheck_guard =
            WebSocketAuthorizationRecheck::new_for_non_websocket_caller(
                dependencies.clone(),
                token.to_owned(),
                client_ip,
                decoded.user_id(),
                Method::POST,
                move |user, api_key| {
                    let project_store_backend_for_access_recheck =
                        Arc::clone(&project_store_backend_for_access_recheck);
                    async move {
                        let (Some(_), Some(key)) = (user, api_key) else {
                            return Err(WebError::invalid_credentials());
                        };
                        if key.project != Some(project) {
                            return Err(WebError::invalid_credentials());
                        }
                        match check_session_access_still_matches_api_key(
                            &project_store_backend_for_access_recheck,
                            key.mask,
                            key.cache_pin,
                        )
                        .await
                        {
                            Ok(true) => Ok(()),
                            _ => Err(WebError::invalid_credentials()),
                        }
                    }
                },
            );
        let project_store_backend_to_close_session_on_access_loss =
            Arc::clone(&project_nix_store_backend);
        Ok(AuthenticatedGrpcCallWithOpenCallSlotGuard {
            project_nix_store_backend,
            open_call_slot_permit,
            caller_lost_access_future: Box::pin(async move {
                open_grpc_store_call_access_recheck_guard
                    .wait_until_access_lost()
                    .await;
                project_store_backend_to_close_session_on_access_loss
                    .session()
                    .closed
                    .cancel();
            }),
        })
    }
}

fn grpc_resource_exhausted_response_for_rate_limit_refusal(
    error: GovernorError,
) -> axum::response::Response {
    match error {
        GovernorError::TooManyRequests { wait_time, .. } => Status::resource_exhausted(format!(
            "rate limit for this network address reached, retry in {wait_time} s"
        )),
        other => Status::internal(other.to_string()),
    }
    .into_http()
}

pub fn add_grpc_nix_store_service_routes_to_router<S, K>(
    router: Router<S>,
    authenticator: GrpcCallerAuthenticator,
    per_network_rate_limit: Arc<GovernorConfig<K, NoOpMiddleware>>,
) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    K: KeyExtractor + Send + Sync + 'static,
    K::Key: Send + Sync + 'static,
{
    let per_network_rate_limit = GovernorLayer::new(per_network_rate_limit)
        .error_handler(grpc_resource_exhausted_response_for_rate_limit_refusal);
    let service = AuthenticatedNixGrpcStoreService::new(Arc::new(authenticator));
    router.merge(
        Router::new()
            .route_service(
                "/nix.remote.NixRemote/{method}",
                service.nix_remote_grpc_service(),
            )
            .route_service(
                "/nix.remote.Scheduler/{method}",
                service.scheduler_grpc_service(),
            )
            .route_layer(per_network_rate_limit),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::FromRef;
    use gradient_entity::api;
    use gradient_fixtures::ids::{project_id, task_id, user_id};
    use gradient_fixtures::mock::mock_db;
    use gradient_fixtures::rows::{project, user};
    use gradient_scheduler::Scheduler;
    use gradient_test_support::state::TestState;
    use gradient_types::ids::RoleId;
    use gradient_types::{ApiId, ProjectId};
    use sea_orm::{DatabaseConnection, MockDatabase, Value};
    use std::collections::BTreeMap;
    use std::time::Duration;
    use tokio::time::timeout;
    use tonic::Code;
    use uuid::Uuid;

    fn grpc_caller_authenticator_from_test_state(test_state: TestState) -> GrpcCallerAuthenticator {
        let state = test_state.build();
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
        GrpcCallerAuthenticator {
            credential_recheck_dependencies: WebSocketCredentialRecheckDependencies::from_ref(
                &state,
            ),
            trusted_proxies: state.config.network.trusted_proxies.clone(),
            handles: SshHandles::from_state(&state, scheduler),
            concurrent_open_grpc_call_limiter_per_credential: Default::default(),
        }
    }

    async fn check_bearer_token_from_client_ip_with_authenticator_and_return_status_error(
        authenticator: &GrpcCallerAuthenticator,
        token: Option<&str>,
        client_ip: &str,
    ) -> Status {
        let mut headers = HeaderMap::new();
        if let Some(token) = token {
            headers.insert(
                "authorization",
                format!("Bearer {token}").parse().expect("header value"),
            );
        }
        let mut extensions = Extensions::new();
        extensions.insert(ConnectInfo(SocketAddr::new(
            client_ip.parse().expect("client ip"),
            443,
        )));
        authenticator
            .check_call_credentials_and_return_store_backend(&headers, &extensions)
            .await
            .err()
            .expect("rejected")
    }

    fn mocked_job_token_resolving_database_row(
        allows_project_credentials_and_privileged_actions: bool,
    ) -> BTreeMap<&'static str, Value> {
        BTreeMap::from([
            ("job", Value::from(Uuid::from_u128(0x7a))),
            ("attempt", Value::from(Uuid::from_u128(0xa1))),
            ("project", Value::from(project_id().into_inner())),
            ("task", Value::from(task_id().into_inner())),
            ("project_name", Value::from("test-project")),
            ("task_name", Value::from("master")),
            ("acting_user", Value::from(user_id().into_inner())),
            (
                "allows_project_credentials_and_privileged_actions",
                Value::from(allows_project_credentials_and_privileged_actions),
            ),
        ])
    }

    fn add_project_store_access_rows_for_job_token_user_to_mock_database(
        db: MockDatabase,
    ) -> MockDatabase {
        let role = gradient_entity::role::Model {
            id: RoleId::now_v7(),
            permission: i64::MAX,
            ..Default::default()
        };
        let access = gradient_entity::project_access::Model {
            project: project_id(),
            user: user_id(),
            role: role.id,
            ..Default::default()
        };
        db.append_query_results([vec![user()]])
            .append_query_results([vec![project()]])
            .append_query_results([vec![access]])
            .append_query_results([vec![role]])
            .append_query_results([Vec::<BTreeMap<&str, Value>>::new()])
    }

    fn mock_database_queued_with_accepted_job_token_store_start_rows_and_project_credentials_and_privileged_actions_flag(
        allows_project_credentials_and_privileged_actions: bool,
    ) -> MockDatabase {
        add_project_store_access_rows_for_job_token_user_to_mock_database(
            mock_db().append_query_results([vec![mocked_job_token_resolving_database_row(
                allows_project_credentials_and_privileged_actions,
            )]]),
        )
    }

    fn append_unchanged_access_recheck_rows_to_mock_database(db: MockDatabase) -> MockDatabase {
        add_project_store_access_rows_for_job_token_user_to_mock_database(
            db.append_query_results([vec![mocked_job_token_resolving_database_row(true)]])
                .append_query_results([vec![user()]]),
        )
    }

    async fn authenticate_job_token_at_grpc_store_with_mocked_rows_and_evaluation_flag(
        allows_project_credentials_and_privileged_actions: bool,
    ) -> Result<AuthenticatedGrpcCallWithOpenCallSlotGuard<SshBackend, OwnedSemaphorePermit>, Status>
    {
        authenticate_job_token_call_on_grpc_store_with_mock_database(
            mock_database_queued_with_accepted_job_token_store_start_rows_and_project_credentials_and_privileged_actions_flag(
                allows_project_credentials_and_privileged_actions,
            )
            .into_connection(),
        )
        .await
    }

    async fn authenticate_job_token_call_on_grpc_store_with_mock_database(
        db: DatabaseConnection,
    ) -> Result<AuthenticatedGrpcCallWithOpenCallSlotGuard<SshBackend, OwnedSemaphorePermit>, Status>
    {
        let authenticator = grpc_caller_authenticator_from_test_state(TestState::new().web_db(db));
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer GRJ_{}", "j".repeat(64))
                .parse()
                .expect("header value"),
        );
        let mut extensions = Extensions::new();
        extensions.insert(ConnectInfo(SocketAddr::new(
            "192.0.2.1".parse().expect("client ip"),
            443,
        )));
        authenticator
            .check_call_credentials_and_return_store_backend(&headers, &extensions)
            .await
    }

    #[tokio::test]
    async fn authenticating_nix_grpc_store_with_exec_job_token_without_allows_project_credentials_and_privileged_actions_is_permission_denied()
     {
        let status =
            authenticate_job_token_at_grpc_store_with_mocked_rows_and_evaluation_flag(false)
                .await
                .err()
                .expect("rejected");
        assert_eq!(status.code(), Code::PermissionDenied, "{status:?}");
    }

    #[tokio::test]
    async fn grpc_nix_store_accepts_exec_job_token_with_project_credentials_and_privileged_actions_flag_and_opens_project_store()
     {
        let accepted =
            authenticate_job_token_at_grpc_store_with_mocked_rows_and_evaluation_flag(true).await;
        assert!(accepted.is_ok(), "{:?}", accepted.err());
    }

    #[tokio::test(start_paused = true)]
    async fn test_open_grpc_store_call_access_lost_future_completes_at_next_sixty_second_recheck_after_job_token_invalidation()
     {
        let db =
            mock_database_queued_with_accepted_job_token_store_start_rows_and_project_credentials_and_privileged_actions_flag(true)
                .append_query_results([Vec::<BTreeMap<&str, Value>>::new()])
                .into_connection();
        let mut call = authenticate_job_token_call_on_grpc_store_with_mock_database(db)
            .await
            .expect("accepted");

        assert!(
            timeout(Duration::from_secs(59), &mut call.caller_lost_access_future)
                .await
                .is_err()
        );
        timeout(Duration::from_secs(2), &mut call.caller_lost_access_future)
            .await
            .expect("the recheck at 60 seconds no longer decodes the job token and ends the call");
        assert!(
            call.project_nix_store_backend
                .session()
                .closed
                .is_cancelled()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn open_grpc_store_call_with_unchanged_access_keeps_access_lost_future_pending_through_three_database_rechecks()
     {
        let mut queued =
            mock_database_queued_with_accepted_job_token_store_start_rows_and_project_credentials_and_privileged_actions_flag(true);
        for _ in 0..3 {
            queued = append_unchanged_access_recheck_rows_to_mock_database(queued);
        }
        let db = queued.into_connection();
        let mut call = authenticate_job_token_call_on_grpc_store_with_mock_database(db.clone())
            .await
            .expect("accepted");

        for _ in 0..3 {
            assert!(
                timeout(Duration::from_secs(61), &mut call.caller_lost_access_future)
                    .await
                    .is_err()
            );
        }
        assert_eq!(db.into_transaction_log().len(), 6 + 3 * 7);
    }

    #[tokio::test(start_paused = true)]
    async fn grpc_store_call_closes_when_job_token_project_differs_at_next_access_recheck() {
        let mut job_token_row_of_other_project = mocked_job_token_resolving_database_row(true);
        job_token_row_of_other_project.insert("project", Value::from(Uuid::now_v7()));
        let db = add_project_store_access_rows_for_job_token_user_to_mock_database(
            mock_database_queued_with_accepted_job_token_store_start_rows_and_project_credentials_and_privileged_actions_flag(true)
                .append_query_results([vec![job_token_row_of_other_project]])
                .append_query_results([vec![user()]]),
        )
        .into_connection();
        let mut call = authenticate_job_token_call_on_grpc_store_with_mock_database(db)
            .await
            .expect("accepted");

        timeout(Duration::from_secs(61), &mut call.caller_lost_access_future)
            .await
            .expect("the recheck at 60 seconds can detect the job token restricted to another project and end the call");
        assert!(
            call.project_nix_store_backend
                .session()
                .closed
                .is_cancelled()
        );
    }

    async fn check_bearer_token_from_client_ip_against_api_key_rows_and_return_status_error(
        api_keys: Vec<api::Model>,
        token: Option<&str>,
        client_ip: &str,
    ) -> Status {
        let db = mock_db().append_query_results([api_keys]).into_connection();
        let authenticator = grpc_caller_authenticator_from_test_state(TestState::new().web_db(db));
        check_bearer_token_from_client_ip_with_authenticator_and_return_status_error(
            &authenticator,
            token,
            client_ip,
        )
        .await
    }

    #[tokio::test]
    async fn api_key_with_too_many_open_grpc_calls_gets_resource_exhausted() {
        let key = api::Model {
            id: ApiId::now_v7(),
            project: Some(ProjectId::now_v7()),
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![key.clone()]])
            .into_connection();
        let authenticator = grpc_caller_authenticator_from_test_state(TestState::new().web_db(db));
        let _open_calls: Vec<_> = (0..MAX_CONCURRENT_GRPC_STORE_CALLS_PER_API_KEY)
            .map(|_| {
                authenticator
                    .concurrent_open_grpc_call_limiter_per_credential
                    .take_open_call_slot_for_credential_or_fail_with_resource_exhausted(
                        RateLimitBucketOwner::ApiKey(key.id),
                    )
                    .expect("slot below the limit")
            })
            .collect();
        let status = check_bearer_token_from_client_ip_with_authenticator_and_return_status_error(
            &authenticator,
            Some(&format!("GRAD{}", "k".repeat(64))),
            "192.0.2.1",
        )
        .await;
        assert_eq!(status.code(), Code::ResourceExhausted);
    }

    #[test]
    fn open_call_limiter_removes_credential_entry_when_last_slot_released_and_counts_credentials_separately()
     {
        let limiter = OpenGrpcStoreCallLimitByCredential::default();
        let first = RateLimitBucketOwner::ApiKey(ApiId::now_v7());
        let second = RateLimitBucketOwner::ApiKey(ApiId::now_v7());
        let first_open_calls: Vec<_> = (0..MAX_CONCURRENT_GRPC_STORE_CALLS_PER_API_KEY)
            .map(|_| {
                limiter
                    .take_open_call_slot_for_credential_or_fail_with_resource_exhausted(first)
                    .expect("slot below the limit")
            })
            .collect();
        assert_eq!(
            limiter
                .take_open_call_slot_for_credential_or_fail_with_resource_exhausted(first)
                .expect_err("over the limit")
                .code(),
            Code::ResourceExhausted
        );
        let second_open_call = limiter
            .take_open_call_slot_for_credential_or_fail_with_resource_exhausted(second)
            .expect("other credential has its own slots");
        drop(first_open_calls);
        drop(second_open_call);
        let third = RateLimitBucketOwner::ApiKey(ApiId::now_v7());
        let _third_open_call = limiter
            .take_open_call_slot_for_credential_or_fail_with_resource_exhausted(third)
            .expect("slot");
        let semaphores = limiter.semaphores.lock().expect("lock");
        assert_eq!(semaphores.len(), 1);
        assert!(semaphores.contains_key(&third));
    }

    #[tokio::test]
    async fn grpc_store_call_without_bearer_token_is_unauthenticated() {
        let status =
            check_bearer_token_from_client_ip_against_api_key_rows_and_return_status_error(
                Vec::new(),
                None,
                "192.0.2.1",
            )
            .await;
        assert_eq!(status.code(), Code::Unauthenticated);
    }

    #[tokio::test]
    async fn reject_unknown_api_key_as_unauthenticated() {
        let status =
            check_bearer_token_from_client_ip_against_api_key_rows_and_return_status_error(
                Vec::new(),
                Some(&format!("GRAD{}", "k".repeat(64))),
                "192.0.2.1",
            )
            .await;
        assert_eq!(status.code(), Code::Unauthenticated);
    }

    #[tokio::test]
    async fn api_key_without_project_restriction_is_rejected_as_unauthenticated() {
        let status =
            check_bearer_token_from_client_ip_against_api_key_rows_and_return_status_error(
                vec![api::Model::default()],
                Some(&format!("GRAD{}", "k".repeat(64))),
                "192.0.2.1",
            )
            .await;
        assert_eq!(status.code(), Code::Unauthenticated);
    }

    #[tokio::test]
    async fn project_api_key_used_from_ip_outside_allowed_list_is_rejected_with_permission_denied()
    {
        let key = api::Model {
            project: Some(ProjectId::now_v7()),
            allowed_ips: Some(vec!["10.0.0.0/8".into()]),
            ..Default::default()
        };
        let status =
            check_bearer_token_from_client_ip_against_api_key_rows_and_return_status_error(
                vec![key],
                Some(&format!("GRAD{}", "k".repeat(64))),
                "192.0.2.1",
            )
            .await;
        assert_eq!(status.code(), Code::PermissionDenied);
    }

    async fn serve_router_on_localhost_and_connect_grpc_channel(
        app: Router,
        server: &mut tokio::task::JoinSet<std::io::Result<()>>,
    ) -> tonic::transport::Channel {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        server.spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
        });
        tonic::transport::Channel::from_shared(format!("http://{address}"))
            .expect("uri")
            .connect()
            .await
            .expect("connect")
    }

    fn global_rate_limit_with_burst(
        burst: u32,
    ) -> Arc<GovernorConfig<tower_governor::key_extractor::GlobalKeyExtractor, NoOpMiddleware>>
    {
        Arc::new(
            tower_governor::governor::GovernorConfigBuilder::default()
                .period(std::time::Duration::from_secs(3600))
                .burst_size(burst)
                .key_extractor(tower_governor::key_extractor::GlobalKeyExtractor)
                .finish()
                .expect("rate limit config"),
        )
    }

    async fn store_info_status(channel: tonic::transport::Channel) -> Status {
        gradient_daemon::grpc_store_server::proto::nix_remote_client::NixRemoteClient::new(channel)
            .store_info(gradient_daemon::grpc_store_server::proto::StoreInfoRequest {})
            .await
            .expect_err("rejected")
    }

    #[tokio::test]
    async fn grpc_client_over_http2_without_tls_without_bearer_token_gets_unauthenticated_status_from_nix_store_routes()
     {
        let app: Router = add_grpc_nix_store_service_routes_to_router(
            Router::new(),
            grpc_caller_authenticator_from_test_state(TestState::new()),
            global_rate_limit_with_burst(100),
        );
        let mut server = tokio::task::JoinSet::new();
        let channel = serve_router_on_localhost_and_connect_grpc_channel(app, &mut server).await;
        assert_eq!(
            store_info_status(channel).await.code(),
            Code::Unauthenticated
        );
    }

    #[tokio::test]
    async fn second_grpc_store_call_over_rate_limit_burst_gets_resource_exhausted_status_instead_of_http_429()
     {
        let app: Router = add_grpc_nix_store_service_routes_to_router(
            Router::new(),
            grpc_caller_authenticator_from_test_state(TestState::new()),
            global_rate_limit_with_burst(1),
        );
        let mut server = tokio::task::JoinSet::new();
        let channel = serve_router_on_localhost_and_connect_grpc_channel(app, &mut server).await;
        assert_eq!(
            store_info_status(channel.clone()).await.code(),
            Code::Unauthenticated
        );
        assert_eq!(
            store_info_status(channel).await.code(),
            Code::ResourceExhausted
        );
    }
}
