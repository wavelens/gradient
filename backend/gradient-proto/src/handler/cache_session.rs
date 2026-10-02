/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::time::Duration;

use gradient_core::ServerState;
use gradient_types::ids::CacheId;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use gradient_wire::messages::{ClientMessage, GradientCapabilities, PROTO_VERSION, ServerMessage};
use gradient_wire::session::frame::Inbound;
use gradient_wire::session::handshake as handshake_fsm;

use super::socket::{HANDSHAKE_TIMEOUT, ProtoSocket, recv_client_msg, send_server_msg};

const CACHE_SESSION_IDLE_TIMEOUT_SECS: u64 = 120;

fn reject_reason(msg: &ClientMessage) -> Option<&'static str> {
    use gradient_wire::types::QueryMode;
    match msg {
        ClientMessage::CacheQuery { mode, .. } => match mode {
            QueryMode::Push => Some("Push not allowed on a read-only cache session"),
            QueryMode::Normal | QueryMode::Pull => None,
        },
        ClientMessage::NarRequest { .. } => None,
        _ => Some("only CacheQuery and NarRequest are allowed on a cache session"),
    }
}

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
        Ok(Some(msg)) => {
            match handshake_fsm::on_init_connection(handshake_fsm::Opening, msg, PROTO_VERSION) {
                Ok(_) => {}
                Err(handshake_fsm::Intent::Reject { code, reason }) => {
                    socket.send_reject(code, reason).await;
                    return;
                }
                Err(_) => return,
            }
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

    let send_chunk_timeout = Duration::from_secs(state.config.nar.send_chunk_timeout_secs);
    let (mut reader, writer) = socket.split(send_chunk_timeout, &state.shutdown);
    let max_serves = state.config.nar.max_concurrent_serves;
    let nar_serve_semaphore = Arc::new(Semaphore::new(max_serves));
    let idle = Duration::from_secs(CACHE_SESSION_IDLE_TIMEOUT_SECS);
    let cancel = state.shutdown.token();

    loop {
        // The idle timeout is only enforced while no NAR transfer is in flight. A client quietly
        // receiving a large download must not be disconnected.
        let next = async {
            if nar_serve_semaphore.available_permits() == max_serves {
                match tokio::time::timeout(idle, recv_client_msg(&mut reader)).await {
                    Ok(m) => m,
                    Err(_) => {
                        debug!(%cache_id, "cache websocket idle timeout; closing");
                        None
                    }
                }
            } else {
                recv_client_msg(&mut reader).await
            }
        };
        let msg = tokio::select! {
            _ = cancel.cancelled() => break,
            m = next => match m {
                Some(Inbound::Control(m)) => m,
                Some(Inbound::Bulk(frame)) => {
                    warn!(%cache_id, variant = frame.variant_name(), "ignoring bulk frame on a read-only cache session");
                    continue;
                }
                None => break,
            },
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
                query_id,
                paths,
                mode,
                ..
            } => {
                let cached = super::cache::query_for_cache(&state, cache_id, &paths, mode).await;
                if send_server_msg(&writer, &ServerMessage::CacheStatus { query_id, cached })
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
                    let Some(slot) =
                        super::nar_serve::ServeSlot::acquire(Arc::clone(&nar_serve_semaphore))
                            .await
                    else {
                        warn!("nar serve semaphore closed");
                        return;
                    };

                    let state = Arc::clone(&state);
                    let writer = writer.clone();
                    let job_id = job_id.clone();
                    let shutdown = state.shutdown.clone();
                    shutdown.spawn(async move {
                        let _slot = slot;
                        if let Err(e) = super::nar_serve::serve_nar_request(
                            &state,
                            &writer,
                            &job_id,
                            &store_path,
                            0,
                            None,
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
    use gradient_wire::types::QueryMode;

    fn cache_query(mode: QueryMode) -> ClientMessage {
        ClientMessage::CacheQuery {
            job_id: "job".into(),
            query_id: "query".into(),
            paths: vec![],
            mode,
            nar_sizes: vec![],
            external: false,
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
            reject_reason(&ClientMessage::UploadRequest {
                job_id: "job".into(),
                request_id: 1,
                object: gradient_wire::types::UploadObject::Nar {
                    store_path: "/nix/store/x".into(),
                },
                size: 1,
            })
            .is_some()
        );
        assert!(reject_reason(&ClientMessage::RequestJobList).is_some());
        assert!(
            reject_reason(&ClientMessage::JobFailed {
                job_id: "job".into(),
                assignment_id: "d".into(),
                error: "x".into(),
                kind: gradient_wire::messages::BuildFailureKind::Permanent,
                missing_paths: vec![],
                spans: vec![],
                elapsed_ms: 0,
            })
            .is_some()
        );
    }
}
