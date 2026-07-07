/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Extension, Path, State};
use axum::response::Response;
use std::sync::Arc;

use crate::access::{CacheAccess, Caller, load_cache};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::client_ip::ClientIp;
use crate::error::{WebError, WebResult};
use gradient_core::ServerState;
use gradient_proto::handler::{PerIpLimiter, ProtoLimiter};

/// `GET /cache/{cache}/proto` - cache-scoped read-only proto WebSocket.
///
/// Authorization is enforced here, at the HTTP layer, via [`load_cache`]:
/// anonymous callers reach public caches only; an API key reaches the private
/// caches it can read (respecting `cache_pin`). Anonymous access additionally
/// requires `allow_anonymous_cache`; private caches always require a key.
#[allow(
    clippy::too_many_arguments,
    reason = "arg-heavy; refactor tracked in #503"
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
    if anonymous && !state.config.proto.allow_anonymous_cache {
        return Err(WebError::forbidden("Anonymous cache access is disabled."));
    }

    let caller = Caller::from_option(&maybe_user);
    let cache = load_cache(
        &state,
        caller,
        api_key.as_ref(),
        cache,
        CacheAccess::Readable,
    )
    .await?;
    let cache_id = cache.id;

    // Every session counts against the global connection cap that protects the
    // server from fd/memory exhaustion; anonymous sessions are additionally
    // capped per source IP.
    let global_permit = global.try_acquire().ok_or_else(|| {
        WebError::service_unavailable("Server is at connection capacity; retry later.")
    })?;
    let ip_permit = if anonymous {
        Some(per_ip.try_acquire(client_ip).ok_or_else(|| {
            WebError::service_unavailable("Too many connections from your IP; retry later.")
        })?)
    } else {
        None
    };

    let upgrade = ws
        .max_message_size(gradient_proto::handler::MAX_PROTO_MESSAGE_SIZE)
        .max_frame_size(gradient_proto::handler::MAX_PROTO_MESSAGE_SIZE);

    Ok(upgrade.on_upgrade(move |sock| async move {
        let _global_permit = global_permit;
        let _ip_permit = ip_permit;
        gradient_proto::handler::handle_cache_socket(
            gradient_proto::server::accept_axum(sock),
            state,
            cache_id,
        )
        .await;
    }))
}
