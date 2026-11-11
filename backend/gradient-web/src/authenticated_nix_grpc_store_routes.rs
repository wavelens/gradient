/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{bearer_token, decode_jwt};
use crate::client_ip::resolve_client_ip;
use crate::ip_allowlist::is_allowed as ip_allowed;
use crate::refusal::SharedRefusal;
use axum::Router;
use axum::extract::ConnectInfo;
use axum::http::StatusCode;
use gradient_core::ServerState;
use gradient_daemon::grpc_store_server::{
    AuthenticatedNixGrpcStoreService, GrpcCallCredentialChecker,
};
use gradient_scheduler::Scheduler;
use gradient_ssh::{SshBackend, SshHandles, open_project_store_backend_for_api_key};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tonic::metadata::MetadataMap;
use tonic::{Extensions, Status};

pub struct GrpcCallerAuthenticator {
    state: Arc<ServerState>,
    handles: SshHandles,
}

impl GrpcCallCredentialChecker for GrpcCallerAuthenticator {
    type Backend = SshBackend;

    async fn check_call_credentials_and_return_store_backend(
        &self,
        metadata: &MetadataMap,
        extensions: &Extensions,
    ) -> Result<Arc<SshBackend>, Status> {
        let headers = metadata.clone().into_headers();
        let token = bearer_token(&headers).ok_or_else(|| {
            Status::unauthenticated(SharedRefusal::AuthorizationRequired.to_string())
        })?;
        let decoded = decode_jwt(
            &self.state.web_db,
            &self.state.jwt_secret,
            &self.state.memory.last_used_stamps,
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

        let peer = extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |info| info.0.ip());
        let client_ip =
            resolve_client_ip(&headers, peer, &self.state.config.network.trusted_proxies);
        if !ip_allowed(client_ip, &context.allowed_ips) {
            return Err(Status::permission_denied(
                SharedRefusal::SourceIpNotAllowed.to_string(),
            ));
        }

        open_project_store_backend_for_api_key(
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
        .ok_or_else(|| Status::permission_denied(SharedRefusal::MissingPermission.to_string()))
    }
}

pub fn add_grpc_nix_store_service_routes_to_router(
    router: Router<Arc<ServerState>>,
    state: &Arc<ServerState>,
    scheduler: Arc<Scheduler>,
) -> Router<Arc<ServerState>> {
    let service = AuthenticatedNixGrpcStoreService::new(Arc::new(GrpcCallerAuthenticator {
        state: Arc::clone(state),
        handles: SshHandles::from_state(state, scheduler),
    }));
    router
        .route_service(
            "/nix.remote.NixRemote/{method}",
            service.nix_remote_grpc_service(),
        )
        .route_service(
            "/nix.remote.Scheduler/{method}",
            service.scheduler_grpc_service(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::api;
    use gradient_fixtures::mock::mock_db;
    use gradient_test_support::state::TestState;
    use gradient_types::ProjectId;
    use tonic::Code;

    async fn check_bearer_token_from_client_ip_against_api_key_rows_and_return_status_error(
        api_keys: Vec<api::Model>,
        token: Option<&str>,
        client_ip: &str,
    ) -> Status {
        let db = mock_db().append_query_results([api_keys]).into_connection();
        let state = TestState::new().web_db(db).build();
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
        let authenticator = GrpcCallerAuthenticator {
            handles: SshHandles::from_state(&state, scheduler),
            state,
        };
        let mut metadata = MetadataMap::new();
        if let Some(token) = token {
            metadata.insert(
                "authorization",
                format!("Bearer {token}").parse().expect("metadata value"),
            );
        }
        let mut extensions = Extensions::new();
        extensions.insert(ConnectInfo(SocketAddr::new(
            client_ip.parse().expect("client ip"),
            443,
        )));
        authenticator
            .check_call_credentials_and_return_store_backend(&metadata, &extensions)
            .await
            .err()
            .expect("rejected")
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

    #[tokio::test]
    async fn grpc_client_over_http2_without_tls_without_bearer_token_gets_unauthenticated_status_from_nix_store_routes()
     {
        let state = TestState::new().build();
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
        let app = add_grpc_nix_store_service_routes_to_router(Router::new(), &state, scheduler)
            .with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let mut server = tokio::task::JoinSet::new();
        server.spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
        });
        let channel = tonic::transport::Channel::from_shared(format!("http://{address}"))
            .expect("uri")
            .connect()
            .await
            .expect("connect");
        let status =
            gradient_daemon::grpc_store_server::proto::nix_remote_client::NixRemoteClient::new(
                channel,
            )
            .store_info(gradient_daemon::grpc_store_server::proto::StoreInfoRequest {})
            .await
            .expect_err("rejected");
        assert_eq!(status.code(), Code::Unauthenticated);
    }
}
