/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Read-only, single-cache websocket session.
//!
//! Authentication happens at the HTTP layer before the upgrade. After a
//! minimal `InitConnection` handshake this session accepts only `CacheQuery`
//! (Normal/Pull) and `NarRequest`, both scoped to one `cache_id`. Every write
//! path - Push, worker registration, job RPCs - is rejected. No scheduler is
//! involved.

use std::sync::Arc;
use std::time::Duration;

use gradient_types::ids::CacheId;
use gradient_core::ServerState;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::messages::{ClientMessage, GradientCapabilities, PROTO_VERSION, ServerMessage};

use super::socket::{HANDSHAKE_TIMEOUT, ProtoSocket, recv_client_msg, send_server_msg};

/// Idle cutoff for a read-only cache session: when no NAR transfer is in
/// flight and no message arrives within this window, the connection is closed
/// so a silent peer cannot pin a connection slot indefinitely.
const CACHE_SESSION_IDLE_TIMEOUT_SECS: u64 = 120;

/// Allow-list classifier: `Some(reason)` means the message is rejected on a
/// read-only cache session, `None` means it is served. Pure so the policy is
/// unit-testable without a socket or DB.
fn reject_reason(msg: &ClientMessage) -> Option<&'static str> {
    use gradient_types::proto::QueryMode;
    match msg {
        ClientMessage::CacheQuery { mode, .. } => match mode {
            QueryMode::Push => Some("Push not allowed on a read-only cache session"),
            QueryMode::Normal | QueryMode::Pull => None,
        },
        ClientMessage::NarRequest { .. } => None,
        _ => Some("only CacheQuery and NarRequest are allowed on a cache session"),
    }
}

/// Read-only capabilities advertised to the peer: cache reads only.
fn readonly_capabilities() -> GradientCapabilities {
    GradientCapabilities {
        core: false,
        build: false,
        eval: false,
        fetch: false,
        cache: true,
        federate: false,
    }
}

pub async fn handle_cache_socket(
    mut socket: ProtoSocket,
    state: Arc<ServerState>,
    cache_id: CacheId,
) {
    info!(%cache_id, "cache websocket session opened");

    match tokio::time::timeout(HANDSHAKE_TIMEOUT, socket.recv_msg()).await {
        Ok(Some(ClientMessage::InitConnection { version, .. })) => {
            if version != PROTO_VERSION {
                socket
                    .send_reject(400, format!("unsupported protocol version {version}"))
                    .await;
                return;
            }
        }
        Ok(Some(_)) => {
            socket.send_error(400, "expected InitConnection".into()).await;
            return;
        }
        Ok(None) => return,
        Err(_) => {
            warn!(%cache_id, "cache websocket handshake timed out");
            return;
        }
    }

    if socket
        .send_msg(&ServerMessage::InitAck {
            version: PROTO_VERSION,
            capabilities: readonly_capabilities(),
            authorized_peers: vec![],
            failed_peers: vec![],
        })
        .await
        .is_err()
    {
        return;
    }

    let send_chunk_timeout = Duration::from_secs(state.config.proto.nar_send_chunk_timeout_secs);
    let (mut reader, writer) = socket.split(send_chunk_timeout);
    let max_serves = state.config.proto.max_concurrent_nar_serves;
    let nar_serve_semaphore = Arc::new(Semaphore::new(max_serves));
    let idle = Duration::from_secs(CACHE_SESSION_IDLE_TIMEOUT_SECS);

    loop {
        // Only enforce the idle timeout while no NAR transfer is in flight, so
        // a client that batches its NarRequests and then quietly receives a
        // large download is not disconnected mid-transfer.
        let msg = if nar_serve_semaphore.available_permits() == max_serves {
            match tokio::time::timeout(idle, recv_client_msg(&mut reader)).await {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(_) => {
                    debug!(%cache_id, "cache websocket idle timeout; closing");
                    break;
                }
            }
        } else {
            match recv_client_msg(&mut reader).await {
                Some(m) => m,
                None => break,
            }
        };

        if let Some(reason) = reject_reason(&msg) {
            warn!(%cache_id, variant = msg.variant_name(), "rejecting message on cache session");
            if send_server_msg(
                &writer,
                &ServerMessage::Reject {
                    code: 403,
                    reason: reason.to_owned(),
                },
            )
            .await
            .is_err()
            {
                break;
            }
            continue;
        }

        match msg {
            ClientMessage::CacheQuery {
                job_id,
                paths,
                mode,
            } => {
                let cached = super::cache::query_for_cache(&state, cache_id, &paths, mode).await;
                if send_server_msg(&writer, &ServerMessage::CacheStatus { job_id, cached })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            ClientMessage::NarRequest { job_id, paths } => {
                for store_path in paths {
                    if !super::cache::path_in_cache(&state, cache_id, &store_path).await {
                        debug!(%cache_id, %store_path, "skipping path not in this cache");
                        continue;
                    }
                    let permit = match Arc::clone(&nar_serve_semaphore).acquire_owned().await {
                        Ok(p) => p,
                        Err(_) => {
                            warn!("nar serve semaphore closed");
                            return;
                        }
                    };
                    let state = Arc::clone(&state);
                    let writer = writer.clone();
                    let job_id = job_id.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        if let Err(e) = super::socket::serve_nar_request(
                            &state, &writer, &job_id, &store_path, 0, None,
                        )
                        .await
                        {
                            debug!(%store_path, error = %e, "cache NAR serve task failed");
                        }
                    });
                }
            }
            _ => {}
        }
    }

    info!(%cache_id, "cache websocket session closed");
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::proto::QueryMode;

    fn cache_query(mode: QueryMode) -> ClientMessage {
        ClientMessage::CacheQuery {
            job_id: "job".into(),
            paths: vec![],
            mode,
        }
    }

    #[test]
    fn allows_reads_rejects_writes() {
        assert!(reject_reason(&cache_query(QueryMode::Normal)).is_none());
        assert!(reject_reason(&cache_query(QueryMode::Pull)).is_none());
        assert!(
            reject_reason(&ClientMessage::NarRequest {
                job_id: "job".into(),
                paths: vec![],
            })
            .is_none()
        );

        assert!(reject_reason(&cache_query(QueryMode::Push)).is_some());
        assert!(
            reject_reason(&ClientMessage::NarPush {
                job_id: "job".into(),
                store_path: "/nix/store/x".into(),
                data: vec![],
                offset: 0,
                is_final: true,
            })
            .is_some()
        );
        assert!(reject_reason(&ClientMessage::RequestJobList).is_some());
        assert!(
            reject_reason(&ClientMessage::JobFailed {
                job_id: "job".into(),
                error: "x".into(),
                kind: crate::messages::BuildFailureKind::Permanent,
                missing_paths: vec![],
            })
            .is_some()
        );
    }
}
