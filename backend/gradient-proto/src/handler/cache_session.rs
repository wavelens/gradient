/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::time::Duration;

use gradient_types::MappingContext;
use gradient_types::ids::CacheId;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use gradient_wire::Rejection;
use gradient_wire::messages::{ClientMessage, GradientCapabilities, ServerMessage};
use gradient_wire::session::handshake as handshake_fsm;

use super::SessionHandles;
use super::socket::{HANDSHAKE_TIMEOUT, ProtoSocket, recv_client_msg, send_server_msg};

const CACHE_SESSION_IDLE_TIMEOUT_SECS: u64 = 120;

fn rejection_for(msg: &ClientMessage) -> Option<Rejection> {
    use gradient_wire::types::QueryMode;
    match msg {
        ClientMessage::CacheQuery { mode, .. } => match mode {
            QueryMode::Push => Some(Rejection::PushOnCacheSession),
            QueryMode::Normal | QueryMode::Pull => None,
        },
        ClientMessage::NarRequest { .. } => None,
        _ => Some(Rejection::NotAllowedOnCacheSession),
    }
}

fn readonly_capabilities() -> GradientCapabilities {
    GradientCapabilities {
        core: false,
        build: false,
        eval: false,
        network: false,
        secrets: false,
        exec: false,
        cache: true,
        federate: false,
    }
}

pub async fn handle_cache_socket(
    mut socket: ProtoSocket,
    handles: SessionHandles,
    cache_id: CacheId,
) {
    info!(%cache_id, "cache websocket session opened");

    match tokio::time::timeout(HANDSHAKE_TIMEOUT, socket.recv_msg()).await {
        Ok(Some(msg)) => match handshake_fsm::on_init_connection(handshake_fsm::Opening, msg) {
            Ok(_) => {}
            Err(handshake_fsm::Intent::Reject(rejection)) => {
                socket.send_reject(rejection).await;
                return;
            }
            Err(_) => return,
        },
        Ok(None) => return,
        Err(_) => {
            warn!(%cache_id, "cache websocket handshake timed out");
            return;
        }
    }

    if socket
        .send_msg(&ServerMessage::InitAck {
            capabilities: readonly_capabilities(),
            authorized_peers: vec![],
            failed_peers: vec![],
        })
        .await
        .is_err()
    {
        return;
    }

    let send_chunk_timeout =
        Duration::from_secs(handles.db.config.nar.send_chunk_timeout_secs.get());
    let (mut reader, writer) = socket.split(send_chunk_timeout, &handles.db.shutdown);
    let max_serves = handles.db.config.nar.max_concurrent_serves.get();
    let nar_serve_semaphore = Arc::new(Semaphore::new(max_serves));
    let idle = Duration::from_secs(CACHE_SESSION_IDLE_TIMEOUT_SECS);
    let cancel = handles.db.shutdown.token();
    let mapper = handles
        .db
        .store_path_mapper(MappingContext::Cache(cache_id));

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
                Some(m) => m,
                None => break,
            },
        };

        if let Some(rejection) = rejection_for(&msg) {
            warn!(%cache_id, variant = msg.variant_name(), "rejecting message on cache session");
            if send_server_msg(&writer, &ServerMessage::from(rejection))
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
                let cached = super::cache::query_for_cache(&handles, cache_id, &paths, mode).await;
                if send_server_msg(&writer, &ServerMessage::CacheStatus { query_id, cached })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            ClientMessage::NarRequest { job_id, paths } => {
                for store_path in paths {
                    if !super::cache::path_in_cache(&handles.cache_db, cache_id, &store_path).await
                    {
                        debug!(%cache_id, %store_path, "skipping path not in this cache");
                        continue;
                    }
                    let Some(slot) = super::nar_serve::ServeSlot::acquire(
                        Arc::clone(&nar_serve_semaphore),
                        Arc::clone(&handles.nar_downloads),
                    )
                    .await
                    else {
                        warn!("nar serve semaphore closed");
                        return;
                    };

                    let handles = handles.clone();
                    let writer = writer.clone();
                    let job_id = job_id.clone();
                    let mapper = mapper.clone();
                    let shutdown = handles.db.shutdown.clone();
                    shutdown.spawn(async move {
                        let _slot = slot;
                        if let Err(e) = super::nar_serve::serve_nar_request(
                            &handles,
                            &writer,
                            &mapper,
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
        assert!(rejection_for(&cache_query(QueryMode::Normal)).is_none());
        assert!(rejection_for(&cache_query(QueryMode::Pull)).is_none());
        assert!(
            rejection_for(&ClientMessage::NarRequest {
                job_id: "job".into(),
                paths: vec![],
            })
            .is_none()
        );

        assert!(rejection_for(&cache_query(QueryMode::Push)).is_some());
        assert!(
            rejection_for(&ClientMessage::UploadRequest {
                job_id: "job".into(),
                request_id: 1,
                object: gradient_wire::types::UploadObject::Nar {
                    store_path: "/nix/store/x".into(),
                },
                size: 1,
            })
            .is_some()
        );
        assert!(rejection_for(&ClientMessage::RequestJobList).is_some());
        assert!(
            rejection_for(&ClientMessage::JobFailed {
                job_id: "job".into(),
                assignment_id: "d".into(),
                error: "x".into(),
                kind: gradient_wire::messages::BuildFailureKind::Permanent,
                missing_paths: vec![],
                spans: vec![],
                elapsed_ms: 0,
                metrics: None,
            })
            .is_some()
        );
    }
}
