/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Extension, Path, State};
use axum::response::Response;
use std::sync::Arc;

use crate::access::{Caller, CacheAccess, load_cache};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::client_ip::ClientIp;
use crate::error::{WebError, WebResult};
use gradient_core::types::ServerState;
use proto::handler::PerIpLimiter;

/// `GET /cache/{cache}/proto` - cache-scoped read-only proto WebSocket.
///
/// Authorization is enforced here, at the HTTP layer, via [`load_cache`]:
/// anonymous callers reach public caches only; an API key reaches the private
/// caches it can read (respecting `cache_pin`). Anonymous access additionally
/// requires `allow_anonymous_cache`; private caches always require a key.
pub async fn cache_proto(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(per_ip): Extension<Arc<PerIpLimiter>>,
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

    let upgrade = ws
        .max_message_size(proto::handler::MAX_PROTO_MESSAGE_SIZE)
        .max_frame_size(proto::handler::MAX_PROTO_MESSAGE_SIZE);

    if anonymous {
        let permit = per_ip.try_acquire(client_ip).ok_or_else(|| {
            WebError::service_unavailable("Too many connections from your IP; retry later.")
        })?;
        Ok(upgrade.on_upgrade(move |sock| async move {
            let _permit = permit;
            proto::handler::handle_cache_socket(proto::server::accept_axum(sock), state, cache_id)
                .await;
        }))
    } else {
        Ok(upgrade.on_upgrade(move |sock| async move {
            proto::handler::handle_cache_socket(proto::server::accept_axum(sock), state, cache_id)
                .await;
        }))
    }
}
