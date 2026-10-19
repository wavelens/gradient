/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::time::Duration;

use gradient_types::ids::CacheId;
use gradient_types::{MappingContext, StorePath, StorePathMapper};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use gradient_wire::Rejection;
use gradient_wire::messages::{ClientMessage, GradientCapabilities, ServerMessage};
use gradient_wire::session::handshake as handshake_fsm;

use super::SessionHandles;
use super::cache::{path_in_cache, query_for_cache};
use super::nar_serve::{ServeSlot, serve_nar_request};
use super::socket::{
    HANDSHAKE_TIMEOUT, ProtoSocket, ProtoWriter, recv_client_msg, send_server_msg,
};
use super::store_paths::{inbound_or_refuse, outbound_message};

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

struct CacheSession {
    handles: SessionHandles,
    writer: ProtoWriter,
    cache_id: CacheId,
    mapper: StorePathMapper,
    nar_serve_semaphore: Arc<Semaphore>,
    max_serves: usize,
}

impl CacheSession {
    fn new(handles: SessionHandles, writer: ProtoWriter, cache_id: CacheId) -> Self {
        let max_serves = handles.db.config.nar.max_concurrent_serves.get();
        let mapper = handles
            .db
            .store_path_mapper(MappingContext::Cache(cache_id));
        Self {
            handles,
            writer,
            cache_id,
            mapper,
            nar_serve_semaphore: Arc::new(Semaphore::new(max_serves)),
            max_serves,
        }
    }

    fn serving_nothing(&self) -> bool {
        self.nar_serve_semaphore.available_permits() == self.max_serves
    }

    async fn serve(&self, msg: ClientMessage) -> bool {
        let cache_id = self.cache_id;
        if let Some(rejection) = rejection_for(&msg) {
            warn!(%cache_id, variant = msg.variant_name(), "rejecting message on cache session");
            return send_server_msg(&self.writer, &ServerMessage::from(rejection))
                .await
                .is_ok();
        }
        let Some(msg) = inbound_or_refuse(&self.writer, &self.mapper, msg).await else {
            return true;
        };

        match msg {
            ClientMessage::CacheQuery {
                query_id,
                paths,
                mode,
                ..
            } => {
                let cached = query_for_cache(&self.handles, cache_id, &paths, mode).await;
                let status = ServerMessage::CacheStatus { query_id, cached };
                send_server_msg(&self.writer, &outbound_message(status, &self.mapper))
                    .await
                    .is_ok()
            }
            ClientMessage::NarRequest { job_id, paths } => self.serve_nars(job_id, paths).await,
            _ => true,
        }
    }

    async fn serve_nars(&self, job_id: String, paths: Vec<String>) -> bool {
        let cache_id = self.cache_id;
        for store_path in paths {
            let in_cache = match StorePath::parse_stored(&store_path) {
                Ok(path) => path_in_cache(&self.handles.cache_db, cache_id, &path).await,
                Err(_) => false,
            };
            if !in_cache {
                debug!(%cache_id, %store_path, "skipping path not in this cache");
                continue;
            }
            let Some(slot) = ServeSlot::acquire(
                Arc::clone(&self.nar_serve_semaphore),
                Arc::clone(&self.handles.nar_downloads),
            )
            .await
            else {
                warn!("nar serve semaphore closed");
                return false;
            };

            let handles = self.handles.clone();
            let writer = self.writer.clone();
            let mapper = self.mapper.clone();
            let job_id = job_id.clone();
            let shutdown = handles.db.shutdown.clone();
            shutdown.spawn(async move {
                let _slot = slot;
                if let Err(e) =
                    serve_nar_request(&handles, &writer, &mapper, &job_id, &store_path, 0, None)
                        .await
                {
                    debug!(%store_path, error = %e, "cache NAR serve task failed");
                }
            });
        }
        true
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
    let idle = Duration::from_secs(CACHE_SESSION_IDLE_TIMEOUT_SECS);
    let cancel = handles.db.shutdown.token();
    let session = CacheSession::new(handles, writer, cache_id);

    loop {
        // The idle timeout is only enforced while no NAR transfer is in flight. A client quietly
        // receiving a large download must not be disconnected.
        let next = async {
            if session.serving_nothing() {
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

        if !session.serve(msg).await {
            break;
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

    #[tokio::test]
    async fn a_cache_socket_query_answers_with_the_paths_asked_for() {
        use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};
        use gradient_test_support::state::TestState;

        let cache_id = CacheId::nil();
        let stored = gradient_entity::cached_path::Model {
            hash: EXAMPLE_STORED_HASH.into(),
            file_hash: Some("sha256:f".into()),
            confirmed: true,
            ..Default::default()
        };
        let cache_db = gradient_fixtures::mock::mock_db()
            .append_query_results([vec![stored.clone()]])
            .append_query_results([vec![gradient_entity::cached_path_signature::Model {
                cached_path: stored.id,
                cache: cache_id,
                ..Default::default()
            }]])
            .append_query_results([vec![stored]])
            .into_connection();
        let state = TestState::new()
            .cache_db(cache_db)
            .store_path_mapping(Arc::new(RotatedHashMapping))
            .build();
        let (writer, mut sent) = ProtoWriter::spy(Duration::from_secs(5));
        let session = CacheSession::new(SessionHandles::from_state(&state), writer, cache_id);
        let query = |query_id: &str, path: String| ClientMessage::CacheQuery {
            job_id: "job".into(),
            query_id: query_id.into(),
            paths: vec![path],
            mode: QueryMode::Normal,
            nar_sizes: vec![],
            external: false,
        };
        let decode = |bytes| {
            gradient_wire::codec::from_bytes::<ServerMessage>(
                bytes,
                *gradient_wire::PROTO_VERSIONS.end(),
            )
            .expect("a server message")
        };
        let asked = format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello");

        assert!(session.serve(query("q1", asked.clone())).await);
        let ServerMessage::CacheStatus { cached, .. } = decode(sent.try_recv().unwrap()) else {
            panic!("a CacheStatus")
        };
        assert_eq!(cached[0].path, asked);

        assert!(session.serve(query("q2", "abc-hello".into())).await);
        assert!(matches!(
            decode(sent.try_recv().unwrap()),
            ServerMessage::CacheStatus { query_id, cached } if query_id == "q2" && cached.is_empty()
        ));
    }
}
