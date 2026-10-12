/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Extension, Path, State};
use axum::response::Response;
use std::sync::Arc;

use super::refusal::CacheRefusal;
use crate::access::{CacheAccess, Caller, load_cache};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::client_ip::ClientIp;
use crate::error::{WebError, WebResult};
use gradient_core::ServerState;
use gradient_wire::session::frame::SAFE_INFLIGHT_MESSAGE_SIZE;
use gradient_wire::{PerIpLimiter, ProtoLimiter};

#[allow(
    clippy::too_many_arguments,
    reason = "arg-heavy; refactor tracked in #503"
)]
#[utoipa::path(
    get,
    path = "/cache/{cache}/proto",
    operation_id = "cacheProtoWebSocket",
    tag = "nix-cache",
    summary = "Cache proto WebSocket",
    description = "Read-only, cache-scoped WebSocket with the `/proto` wire format. Accepts only `CacheQuery` (Normal and Pull) and `NarRequest`. Public caches allow anonymous sessions (`GRADIENT_PROTO_ANONYMOUS_CACHE_ENABLE`), capped per IP by `GRADIENT_PROTO_ANONYMOUS_CACHE_MAX_CONNECTIONS_PER_IP`; private caches need `Authorization: Bearer GRAD<key>`.",
    params(("cache" = String, Path)),
    responses((status = SWITCHING_PROTOCOLS, description = "Switching Protocols - WebSocket upgrade successful"), WebError)
)]
pub async fn cache_proto(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(per_ip): Extension<Arc<PerIpLimiter>>,
    Extension(global): Extension<Arc<ProtoLimiter>>,
    Extension(ClientIp(client_ip)): Extension<ClientIp>,
    Path(cache): Path<String>,
    ws: WebSocketUpgrade,
) -> WebResult<Response> {
    let anonymous = maybe_user.is_none() && api_key.as_ref().is_none();
    if anonymous && !state.config.proto.anonymous_cache_enable {
        return Err(CacheRefusal::AnonymousAccessDisabled.into());
    }

    let caller = Caller::from(&maybe_user);
    let cache = load_cache(
        &state.db(),
        caller,
        api_key.as_ref(),
        cache,
        CacheAccess::Readable,
    )
    .await?;
    let cache_id = cache.id;

    let global_permit = global
        .try_acquire()
        .ok_or(CacheRefusal::ConnectionCapacityReached)?;
    let ip_permit = if anonymous {
        Some(
            per_ip
                .try_acquire(client_ip)
                .ok_or(CacheRefusal::TooManyConnectionsFromIp)?,
        )
    } else {
        None
    };

    let upgrade = ws
        .max_message_size(SAFE_INFLIGHT_MESSAGE_SIZE)
        .max_frame_size(SAFE_INFLIGHT_MESSAGE_SIZE);

    let shutdown = state.shutdown.clone();
    let handles = gradient_proto::handler::SessionHandles::from_state(&state);
    Ok(upgrade.on_upgrade(move |sock| async move {
        let _ = shutdown
            .spawn(async move {
                let _global_permit = global_permit;
                let _ip_permit = ip_permit;
                gradient_proto::handler::handle_cache_socket(
                    gradient_wire::server::accept_axum(sock),
                    handles,
                    cache_id,
                )
                .await;
            })
            .await;
    }))
}
