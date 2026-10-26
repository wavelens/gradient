/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[macro_use]
pub mod patch;

pub mod access;
pub mod api_spec;
pub mod api_typescript;
pub mod audit;
pub mod authorization;
pub(crate) mod client_ip;
pub mod endpoints;
pub mod error;
pub mod helpers;
pub mod invite_policy;
pub mod ip_allowlist;
pub mod json_body;
pub mod lookup;
pub mod metrics_scope;
pub mod names;
pub mod otlp;
pub mod permissions;
pub mod refusal;
pub mod resource;
mod routes;
pub mod scim;
pub mod scope;
mod store_paths;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::extract::{ConnectInfo, DefaultBodyLimit, MatchedPath};
use axum::{Router, middleware};
use bytes::Bytes;
use governor::middleware::NoOpMiddleware;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderMap, HeaderValue, Request, Response};
use ipnet::IpNet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tower_governor::GovernorLayer;
use tower_governor::errors::GovernorError;
use tower_governor::governor::{GovernorConfig, GovernorConfigBuilder};
use tower_governor::key_extractor::KeyExtractor;
use tower_http::classify::ServerErrorsFailureClass;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::request_id::{
    MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use tower_http::trace::TraceLayer;
use tracing::Span;
use uuid::Uuid;

use endpoints::{handle_404, metrics};
use gradient_core::{InitError, ServerState};
use gradient_scheduler::Scheduler;
use gradient_util::supervision::ChildSpec;
use gradient_wire::{PerIpLimiter, ProtoLimiter};
use std::sync::Arc;

const NAR_UPLOAD_CHUNK_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
struct ClientIpKey {
    trusted_proxies: Arc<[IpNet]>,
}

impl KeyExtractor for ClientIpKey {
    type Key = IpAddr;

    fn extract<T>(&self, req: &Request<T>) -> Result<Self::Key, GovernorError> {
        let extensions = req.extensions();
        let peer = extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0)
            .or_else(|| extensions.get::<MockConnectInfo<SocketAddr>>().map(|m| m.0));
        Ok(peer.map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |peer| {
            client_ip::resolve_client_ip(req.headers(), peer.ip(), &self.trusted_proxies)
        }))
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct MakeRequestUuid;

impl MakeRequestId for MakeRequestUuid {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        let id = Uuid::now_v7().to_string();
        let value = HeaderValue::from_str(&id).ok()?;
        Some(RequestId::new(value))
    }
}

fn rate_limit(
    key: &ClientIpKey,
    refill: Duration,
    burst: u32,
) -> Result<Arc<GovernorConfig<ClientIpKey, NoOpMiddleware>>, InitError> {
    let config = GovernorConfigBuilder::default()
        .period(refill)
        .burst_size(burst)
        .key_extractor(key.clone())
        .finish()
        .ok_or_else(|| InitError::NetworkConfig("invalid rate-limit configuration".into()))?;

    Ok(Arc::new(config))
}

pub fn create_router(state: Arc<ServerState>) -> Result<Router, InitError> {
    let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
    create_router_with_scheduler(state, scheduler)
}

pub fn create_router_with_scheduler(
    state: Arc<ServerState>,
    scheduler: Arc<Scheduler>,
) -> Result<Router, InitError> {
    let serve_url: http::HeaderValue =
        state
            .config
            .server
            .serve_url
            .clone()
            .try_into()
            .map_err(|_| {
                InitError::NetworkConfig(format!(
                    "invalid serve_url: {}",
                    state.config.server.serve_url
                ))
            })?;
    let debug_url: http::HeaderValue =
        format!("http://{}:8000", state.config.server.listen_addr.clone())
            .try_into()
            .map_err(|_| {
                InitError::NetworkConfig(format!(
                    "invalid debug_url from ip {}",
                    state.config.server.listen_addr
                ))
            })?;

    let cors_allow_origin = AllowOrigin::list(vec![serve_url, debug_url]);

    let cors = CorsLayer::new()
        .allow_origin(cors_allow_origin)
        .allow_headers(vec![AUTHORIZATION, ACCEPT, CONTENT_TYPE])
        .allow_credentials(true);

    let trace = TraceLayer::new_for_http()
        .make_span_with(|request: &Request<Body>| {
            let request_id = request
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let route = request
                .extensions()
                .get::<MatchedPath>()
                .map(MatchedPath::as_str)
                .unwrap_or_else(|| request.uri().path());
            tracing::info_span!(
                "http_request",
                method = %request.method(),
                route = %route,
                request_id = %request_id,
            )
        })
        .on_request(|request: &Request<Body>, _span: &Span| {
            tracing::debug!(
                path = request.uri().path(),
                "request started",
            )
        })
        .on_response(
            |_response: &Response<Body>, latency: Duration, _span: &Span| {
                tracing::debug!(latency_ms = u64::try_from(latency.as_millis()).unwrap_or(u64::MAX), "response generated")
            },
        )
        .on_body_chunk(|chunk: &Bytes, _latency: Duration, _span: &Span| {
            tracing::debug!(bytes = chunk.len(), "sending chunk")
        })
        .on_eos(
            |_trailers: Option<&HeaderMap>, stream_duration: Duration, _span: &Span| {
                tracing::debug!(
                    duration_ms = u64::try_from(stream_duration.as_millis()).unwrap_or(u64::MAX),
                    "stream closed",
                )
            },
        )
        .on_failure(
            |error: ServerErrorsFailureClass, _latency: Duration, _span: &Span| {
                tracing::debug!(error = ?error, "request failed")
            },
        );

    let key = ClientIpKey {
        trusted_proxies: state.config.network.trusted_proxies.clone().into(),
    };
    let rate_limit = |refill, burst| rate_limit(&key, refill, burst);

    let limits = routes::BodyLimits::from_config(&state.config);
    let (auth_api, _) = routes::auth_api(&limits).split_for_parts();
    let auth_api = auth_api.route_layer(middleware::from_fn_with_state(
        Arc::clone(&state),
        authorization::authorize,
    ));

    let (optional_api, _) = routes::optional_api().split_for_parts();
    let optional_api = optional_api.route_layer(middleware::from_fn_with_state(
        Arc::clone(&state),
        authorization::authorize_optional,
    ));

    let (auth_sensitive, _) = routes::auth_sensitive().split_for_parts();
    let auth_sensitive =
        auth_sensitive.route_layer(GovernorLayer::new(rate_limit(Duration::from_secs(6), 5)?));

    let (webhook_routes, _) = routes::webhook_routes().split_for_parts();
    let webhook_routes =
        webhook_routes.route_layer(GovernorLayer::new(rate_limit(Duration::from_secs(1), 30)?));

    let (open_api, _) = routes::open_api().split_for_parts();

    let api = Router::new()
        .merge(auth_api)
        .merge(optional_api)
        .merge(auth_sensitive)
        .merge(webhook_routes)
        .merge(open_api);

    scheduler.start();
    state.shutdown.supervise(ChildSpec::pass(
        gradient_db::maintenance::retention::Retention::new(state.db()),
    ));
    state
        .shutdown
        .supervise(ChildSpec::pass(gradient_db::metrics::rollup::Rollup::new(
            state.db(),
        )));
    state.shutdown.supervise(ChildSpec::pass(
        gradient_db::metrics::cache_traffic::CacheMetricFlush::new(
            state.db(),
            Arc::clone(&state.memory.cache_traffic),
        ),
    ));
    gradient_db::metrics::cache_traffic::flush_on_shutdown(
        state.db(),
        Arc::clone(&state.memory.cache_traffic),
    );
    state.shutdown.supervise(ChildSpec::pass(
        gradient_db::metrics::infra::InfraMetricFlush::new(state.db()),
    ));
    gradient_db::metrics::infra::flush_on_shutdown(state.db());
    otlp::start_otlp(Arc::clone(&state), Arc::clone(&scheduler));
    let sessions = gradient_proto::SessionsHandle::new();
    state
        .shutdown
        .supervise(sessions.child_spec(Arc::clone(&scheduler)));
    gradient_proto::outbound::start_outbound_loop(
        Arc::clone(&state),
        Arc::clone(&scheduler),
        Arc::clone(&sessions),
    );

    let proto_limiter = Arc::new(ProtoLimiter::new(state.config.proto.max_connections.get()));

    let (cache_upload_api, _) = routes::cache_upload_api(&limits).split_for_parts();
    let cache_upload_api = cache_upload_api
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authorization::authorize,
        ))
        .route_layer(GovernorLayer::new(rate_limit(
            Duration::from_millis(2),
            10_000,
        )?));

    let api = api
        .route_layer(GovernorLayer::new(rate_limit(
            Duration::from_millis(200),
            150,
        )?))
        .merge(cache_upload_api);
    let api = api.route_layer(axum::middleware::from_fn(metrics::track_http_metrics));
    let api = api.layer(DefaultBodyLimit::max(
        state.config.http.max_request_size.get(),
    ));

    let mut app = Router::new()
        .nest("/api/v1", api)
        .merge(
            routes::proto_api()
                .split_for_parts()
                .0
                .route_layer(GovernorLayer::new(rate_limit(
                    Duration::from_millis(200),
                    150,
                )?)),
        )
        .layer(axum::Extension(Arc::clone(&scheduler)))
        .layer(axum::Extension(Arc::clone(&proto_limiter)))
        .layer(axum::Extension(Arc::clone(&sessions)));

    if state.config.metrics.is_some() {
        let (metrics_route, _) = routes::metrics_api().split_for_parts();
        let metrics_route = metrics_route
            .route_layer(middleware::from_fn_with_state(
                Arc::clone(&state),
                endpoints::metrics::metrics_auth,
            ))
            .route_layer(GovernorLayer::new(rate_limit(Duration::from_secs(1), 5)?))
            .layer(axum::Extension(Arc::clone(&scheduler)));
        app = app.merge(metrics_route);
    }

    let http = &state.config.http;
    let nar_cache_limit = || {
        rate_limit(
            Duration::from_secs(1) / http.cache_requests_per_second.get(),
            http.cache_request_burst.get(),
        )
    };
    let (cache_routes, _) = routes::cache_routes().split_for_parts();
    let cache_routes = cache_routes.route_layer(GovernorLayer::new(nar_cache_limit()?));

    let (cache_inspect, _) = routes::cache_inspect().split_for_parts();
    let cache_inspect = cache_inspect.route_layer(GovernorLayer::new(rate_limit(
        Duration::from_millis(333),
        180,
    )?));

    let (cache_log, _) = routes::cache_log().split_for_parts();
    let cache_log = cache_log.route_layer(GovernorLayer::new(rate_limit(
        Duration::from_millis(333),
        900,
    )?));

    let cache_per_ip = Arc::new(PerIpLimiter::new(
        state
            .config
            .proto
            .anonymous_cache_max_connections_per_ip
            .get(),
    ));
    let (cache_proto_route, _) = routes::cache_proto_route().split_for_parts();
    let cache_proto_route = cache_proto_route
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authorization::authorize_optional,
        ))
        .route_layer(GovernorLayer::new(nar_cache_limit()?))
        .layer(axum::Extension(cache_per_ip))
        .layer(axum::Extension(Arc::clone(&proto_limiter)))
        .layer(axum::Extension(Arc::clone(&sessions)));

    app = app
        .merge(cache_routes)
        .merge(cache_inspect)
        .merge(cache_log)
        .merge(cache_proto_route);

    if state.config.scim.is_some() {
        let (scim_routes, _) = routes::scim_routes().split_for_parts();
        let scim_routes = scim_routes.route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            authorization::authorize_scim,
        ));
        app = app.nest("/scim/v2", scim_routes);
    }

    Ok(app
        .fallback(handle_404)
        .layer(cors)
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(trace)
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state))
}

/// Nagle is disabled on every accepted connection. It would hold a small `/proto` control frame
/// back until the peer's delayed ACK. The concrete return type is required because
/// `ConnectInfo<SocketAddr>` is resolving through an impl for `TapIo<L, F>`.
fn tuned_listener(
    listener: tokio::net::TcpListener,
) -> axum::serve::TapIo<tokio::net::TcpListener, fn(&mut tokio::net::TcpStream)> {
    use axum::serve::ListenerExt;
    fn tap(stream: &mut tokio::net::TcpStream) {
        gradient_util::net::disable_nagle(stream);
    }
    listener.tap_io(tap as fn(&mut tokio::net::TcpStream))
}

pub async fn serve_web(state: Arc<ServerState>, scheduler: Arc<Scheduler>) -> std::io::Result<()> {
    let server_url = format!(
        "{}:{}",
        state.config.server.listen_addr.clone(),
        state.config.server.port.clone()
    );

    match gradient_db::maintenance::recovery::recover_interrupted_work(&state.worker_db).await {
        Ok(r)
            if !r.kept.is_empty()
                || r.attempts_aborted > 0
                || r.builds_requeued > 0
                || r.builds_unpromoted > 0
                || r.evals_requeued > 0
                || r.cluster_attempts_closed > 0
                || r.connections_closed > 0 =>
        {
            tracing::warn!(
                assignments_kept = r.kept.len(),
                attempts_aborted = r.attempts_aborted,
                builds_requeued = r.builds_requeued,
                builds_unpromoted = r.builds_unpromoted,
                evals_requeued = r.evals_requeued,
                cluster_attempts_closed = r.cluster_attempts_closed,
                clusters_requeued = r.clusters_requeued,
                clusters_aborted = r.clusters_aborted,
                clusters_failed = r.clusters_failed,
                connections_closed = r.connections_closed,
                "recovered interrupted work from previous process"
            )
        }
        Ok(_) => {}
        Err(e) => tracing::error!(error = ?e, "failed to recover interrupted work"),
    }

    match gradient_db::evaluations::draining::resume_drained_evals(&state.worker_db).await {
        Ok(n) if n > 0 => {
            tracing::warn!(evaluations = n, "recovered evaluations parked by draining")
        }
        Ok(_) => {}
        Err(e) => tracing::error!(error = ?e, "failed to recover draining-parked evaluations"),
    }

    let app = create_router_with_scheduler(Arc::clone(&state), scheduler)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    let listener = tokio::net::TcpListener::bind(&server_url)
        .await
        .map_err(|e| {
            tracing::error!(addr = %server_url, error = %e, "Failed to bind listener");
            e
        })?;
    let listener = tuned_listener(listener);
    sd_notify::notify(&[sd_notify::NotifyState::Ready])?;

    let shutdown = state.shutdown.clone();
    install_signal_handler(shutdown.clone());

    let drain_token = shutdown.token();
    let serve_result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move { drain_token.cancelled().await })
    .await;

    shutdown
        .cancel_and_drain(std::time::Duration::from_secs(30))
        .await;

    serve_result
}

fn install_signal_handler(shutdown: gradient_util::shutdown::Shutdown) {
    let trigger = shutdown.clone();
    shutdown.spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigterm = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "failed to install SIGTERM handler");
                    return;
                }
            };
            tokio::select! {
                _ = tokio::signal::ctrl_c() => tracing::info!("received SIGINT, shutting down"),
                _ = sigterm.recv() => tracing::info!("received SIGTERM, shutting down"),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("received Ctrl-C, shutting down");
        }
        trigger.cancel();
    });
}

#[cfg(test)]
mod tests {
    use super::tuned_listener;
    use axum::serve::Listener as _;
    use tokio::net::{TcpListener, TcpStream};

    #[tokio::test]
    async fn accepted_connections_have_nagle_disabled() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let mut listener = tuned_listener(listener);

        let (accepted, client) = tokio::join!(listener.accept(), TcpStream::connect(addr));
        let (io, _peer) = accepted;
        let _client = client.expect("connect");

        assert!(io.nodelay().expect("read nodelay"));
    }
}
