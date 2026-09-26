/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! One actor per worker connection. The reader task delivers each inbound
//! frame with a call and reads the next only after the reply, so the mailbox
//! never holds more than one frame plus signals and TCP backpressure holds.
//! Liveness is therefore the reader's to stamp, not the handler's, and it keeps
//! stamping while a frame is in flight: the heartbeat behind that frame sits
//! unread in the socket, so a handler waiting on a slow graph would otherwise
//! read as a silent worker and get a healthy connection unregistered.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use gradient_core::ServerState;
use gradient_scheduler::Scheduler;
use gradient_scheduler::actor::{SessionPort, SessionSignal};
use gradient_types::ids::ProjectId;
use ractor::rpc::CallResult;
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use super::dispatch::{ActiveJob, DispatchContext};
use super::eval_cache::EvalCacheReceiveStore;
use super::job_events::{JobEvents, SchedulerJobEvents};
use super::nar_transfer::NarReceiveStore;
use super::session::on_reauth_notify;
use super::socket::{
    JOB_OFFER_CHUNK_SIZE, ProtoSocket, ProtoWriter, recv_client_msg, send_server_msg,
};
use gradient_wire::messages::{ClientMessage, GradientCapabilities, ServerMessage};
use gradient_wire::session::frame::{Inbound, ProtoReader};

/// How long a draining session waits for its in-flight jobs before closing.
pub const SESSION_DRAIN_BUDGET: Duration = Duration::from_secs(20);

/// How often the reader re-stamps liveness while the handler holds a frame;
/// well inside the worker's 10 s heartbeat.
const IN_FLIGHT_STAMP: Duration = Duration::from_secs(5);

pub enum SessionMsg {
    Frame(Inbound<ClientMessage>, RpcReplyPort<bool>),
    Signal(SessionSignal),
    Reattach,
    ReaderClosed,
    DrainDeadline,
}

#[derive(Clone)]
pub struct SessionRef(pub ActorRef<SessionMsg>);

impl SessionPort for SessionRef {
    fn signal(&self, signal: SessionSignal) {
        let _ = self.0.send_message(SessionMsg::Signal(signal));
    }
}

pub struct SessionArgs {
    pub peer_id: String,
    pub state: Arc<ServerState>,
    pub scheduler: Arc<Scheduler>,
    pub socket: ProtoSocket,
    pub capabilities: GradientCapabilities,
    pub authorized_peers: HashSet<ProjectId>,
}

pub struct SessionState {
    peer_id: String,
    state: Arc<ServerState>,
    scheduler: Arc<Scheduler>,
    writer: ProtoWriter,
    capabilities: GradientCapabilities,
    authorized_peers: HashSet<ProjectId>,
    nar: NarReceiveStore,
    eval_cache: EvalCacheReceiveStore,
    nar_serve_semaphore: Arc<Semaphore>,
    offers_seen: u64,
    active: HashMap<String, ActiveJob>,
    job_events: JobEvents,
    draining: bool,
    reader: JoinHandle<()>,
}

pub struct SessionActor;

impl Actor for SessionActor {
    type Msg = SessionMsg;
    type State = SessionState;
    type Arguments = SessionArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let SessionArgs {
            peer_id,
            state,
            scheduler,
            mut socket,
            capabilities,
            authorized_peers,
        } = args;
        let port: Arc<dyn SessionPort> = Arc::new(SessionRef(myself.clone()));
        let registered = match scheduler
            .register_worker(
                &peer_id,
                capabilities.clone(),
                authorized_peers.clone(),
                port,
            )
            .await
        {
            Ok(registered) => registered,
            Err(e) => {
                warn!(%peer_id, error = %e, "registration failed; scheduler unavailable");
                socket
                    .send_reject(503, "scheduler unavailable".into())
                    .await;
                return Err(ActorProcessingErr::from(e.to_string()));
            }
        };

        let proto_cfg = &state.config.proto;
        let send_chunk_timeout = Duration::from_secs(proto_cfg.nar_send_chunk_timeout_secs);
        let partial_ttl = Duration::from_secs(proto_cfg.nar_partial_ttl_secs);
        let max_partial_bytes = proto_cfg.max_nar_buffer_bytes as u64;
        let max_serves = proto_cfg.max_concurrent_nar_serves;
        let partial_root =
            std::path::PathBuf::from(format!("{}/nar-partial", state.config.storage.base_path));
        let nar = NarReceiveStore::new(
            partial_root,
            &peer_id,
            partial_ttl,
            max_partial_bytes,
            state.config.storage.small_nar_bytes,
            state.shutdown.clone(),
        )
        .unwrap_or_else(|e| {
            error!(%peer_id, error = %e, "failed to init NAR partial dir; falling back to temp");
            NarReceiveStore::new(
                std::env::temp_dir().join("gradient-nar-partial"),
                &peer_id,
                partial_ttl,
                max_partial_bytes,
                state.config.storage.small_nar_bytes,
                state.shutdown.clone(),
            )
            .expect("temp partial dir must be creatable")
        });
        let (reader, writer) = socket.split(send_chunk_timeout, &state.shutdown);
        let reader = state.shutdown.spawn(read_loop(
            reader,
            myself,
            registered.last_seen,
            IN_FLIGHT_STAMP,
        ));
        let job_events = JobEvents::spawn(
            &state.shutdown,
            &peer_id,
            SchedulerJobEvents {
                shutdown: state.shutdown.clone(),
                scheduler: Arc::clone(&scheduler),
                writer: writer.clone(),
                peer_id: peer_id.clone(),
            },
        );

        Ok(SessionState {
            peer_id,
            state,
            scheduler,
            writer,
            capabilities,
            authorized_peers,
            nar,
            eval_cache: EvalCacheReceiveStore::new(max_partial_bytes),
            nar_serve_semaphore: Arc::new(Semaphore::new(max_serves)),
            offers_seen: 0,
            active: HashMap::new(),
            job_events,
            draining: false,
            reader,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        st: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SessionMsg::Frame(inbound, reply) => {
                let keep = {
                    let mut ctx = DispatchContext {
                        writer: &st.writer,
                        state: &st.state,
                        scheduler: &st.scheduler,
                        peer_id: &st.peer_id,
                        nar_serve_semaphore: &st.nar_serve_semaphore,
                        active: &mut st.active,
                        job_events: &st.job_events,
                    };

                    ctx.dispatch(inbound, &mut st.nar, &mut st.eval_cache).await
                };
                let _ = reply.send(keep);

                if !keep {
                    myself.stop(Some("peer closed".into()));
                } else if st.draining && st.active.is_empty() {
                    myself.stop(Some("drained".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::Offers(generation)) => {
                if generation > st.offers_seen && !st.draining && !offer_jobs(st).await {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::Reauth) => {
                if !on_reauth_notify(&st.writer, &st.state, &st.peer_id).await {
                    myself.stop(Some("deactivated".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::Abort { job_id, reason }) => {
                info!(peer_id = %st.peer_id, %job_id, %reason, "sending AbortJob to worker");
                if send_server_msg(&st.writer, &ServerMessage::AbortJob { job_id, reason })
                    .await
                    .is_err()
                {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::Close { reason }) => {
                warn!(peer_id = %st.peer_id, %reason, "closing session at the scheduler's request");
                myself.stop(Some(reason));
            }
            SessionMsg::Signal(SessionSignal::Drain) => {
                if st.draining {
                    return Ok(());
                }

                st.draining = true;
                info!(peer_id = %st.peer_id, active = st.active.len(), "draining session");
                let _ = send_server_msg(&st.writer, &ServerMessage::Draining).await;
                st.scheduler.mark_worker_draining(&st.peer_id).await;

                if st.active.is_empty() {
                    myself.stop(Some("drained".into()));
                } else {
                    myself.send_after(SESSION_DRAIN_BUDGET, || SessionMsg::DrainDeadline);
                }
            }
            SessionMsg::DrainDeadline => {
                warn!(peer_id = %st.peer_id, active = st.active.len(), "drain budget expired; closing with jobs in flight");
                myself.stop(Some("drain deadline".into()));
            }
            SessionMsg::Reattach => {
                let port: Arc<dyn SessionPort> = Arc::new(SessionRef(myself.clone()));
                let active = st
                    .active
                    .iter()
                    .map(|(id, job)| (id.clone(), job.pending.clone()))
                    .collect();
                if let Err(e) = st
                    .scheduler
                    .reattach_worker(
                        &st.peer_id,
                        st.capabilities.clone(),
                        st.authorized_peers.clone(),
                        port,
                        active,
                    )
                    .await
                {
                    warn!(peer_id = %st.peer_id, error = %e, "re-registration after scheduler restart failed; closing");
                    myself.stop(Some("reattach failed".into()));
                }
            }
            SessionMsg::ReaderClosed => myself.stop(Some("peer closed".into())),
        }

        Ok(())
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        st: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        st.reader.abort();
        st.job_events.finish().await;
        st.scheduler.unregister_worker(&st.peer_id).await;
        info!(peer_id = %st.peer_id, "WebSocket connection closed");
        Ok(())
    }
}

async fn offer_jobs(st: &mut SessionState) -> bool {
    let offer = st.scheduler.get_new_job_candidates(&st.peer_id).await;
    st.offers_seen = offer.generation;
    if offer.candidates.is_empty() {
        return true;
    }

    debug!(peer_id = %st.peer_id, count = offer.candidates.len(), "pushing job offer (delta)");
    for chunk in offer.candidates.chunks(JOB_OFFER_CHUNK_SIZE) {
        if send_server_msg(
            &st.writer,
            &ServerMessage::JobOffer {
                candidates: chunk.to_vec(),
            },
        )
        .await
        .is_err()
        {
            return false;
        }
    }

    true
}

/// Deliver frames one at a time, stamping the worker's liveness on receipt and
/// every `stamp_every` until the handler answers. A worker that is talking is
/// alive whether or not the server has finished answering it, so the stamp
/// belongs here rather than in the handler.
async fn read_loop(
    mut reader: ProtoReader,
    session: ActorRef<SessionMsg>,
    last_seen: Arc<AtomicI64>,
    stamp_every: Duration,
) {
    let stamp = || {
        last_seen.store(
            gradient_types::now().and_utc().timestamp_millis(),
            Ordering::Relaxed,
        )
    };
    while let Some(inbound) = recv_client_msg(&mut reader).await {
        stamp();
        let handled = session.call(|reply| SessionMsg::Frame(inbound, reply), None);
        tokio::pin!(handled);
        let mut in_flight =
            tokio::time::interval_at(tokio::time::Instant::now() + stamp_every, stamp_every);
        let result = loop {
            tokio::select! {
                result = &mut handled => break result,
                _ = in_flight.tick() => stamp(),
            }
        };
        if !matches!(result, Ok(CallResult::Success(true))) {
            return;
        }
    }

    let _ = session.send_message(SessionMsg::ReaderClosed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{SinkExt, StreamExt};
    use gradient_test_support::prelude::*;
    use gradient_wire::session::frame::WireMessage;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::Notify;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

    /// Stands in for a session whose handler is busy: it answers the frame
    /// without ever stamping liveness, so only the reader can have done it.
    struct DecliningSession;

    impl Actor for DecliningSession {
        type Msg = SessionMsg;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            msg: Self::Msg,
            _st: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let SessionMsg::Frame(_, reply) = msg {
                let _ = reply.send(false);
            }

            Ok(())
        }
    }

    /// Stands in for a session whose handler is stuck on a slow graph: it
    /// holds the frame's reply until `DrainDeadline` releases it.
    struct HoldingSession;

    impl Actor for HoldingSession {
        type Msg = SessionMsg;
        type State = (Arc<Notify>, Option<RpcReplyPort<bool>>);
        type Arguments = Arc<Notify>;

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            received: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok((received, None))
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            msg: Self::Msg,
            (received, held): &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            match msg {
                SessionMsg::Frame(_, reply) => {
                    *held = Some(reply);
                    received.notify_one();
                }
                SessionMsg::DrainDeadline => {
                    if let Some(reply) = held.take() {
                        let _ = reply.send(false);
                    }
                }
                _ => {}
            }

            Ok(())
        }
    }

    async fn connected_pair() -> (ProtoSocket, WebSocketStream<MaybeTlsStream<TcpStream>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let dial = tokio_tungstenite::connect_async(format!("ws://{addr}/proto"));
        let accept = async {
            let (tcp, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(MaybeTlsStream::Plain(tcp))
                .await
                .unwrap()
        };
        let (client, server) = tokio::join!(dial, accept);

        (
            ProtoSocket::Tungstenite(Box::new(server)),
            client.unwrap().0,
        )
    }

    #[tokio::test]
    async fn drain_sends_draining_and_closes_an_idle_session() {
        let (socket, mut client) = connected_pair().await;
        let state = test_state(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
        scheduler.spawn_core(None).await.unwrap();
        let (actor, join) = Actor::spawn(
            None,
            SessionActor,
            SessionArgs {
                peer_id: "w1".into(),
                state: Arc::clone(&state),
                scheduler: Arc::clone(&scheduler),
                socket,
                capabilities: GradientCapabilities::default(),
                authorized_peers: HashSet::new(),
            },
        )
        .await
        .unwrap();
        assert!(scheduler.is_worker_connected("w1").await);

        actor
            .send_message(SessionMsg::Signal(SessionSignal::Drain))
            .unwrap();

        let frame = client
            .next()
            .await
            .expect("a frame")
            .expect("no transport error");
        let Message::Binary(bytes) = frame else {
            panic!("expected a binary frame, got {frame:?}");
        };
        assert!(matches!(
            ServerMessage::decode(bytes)
                .unwrap()
                .into_message()
                .unwrap(),
            ServerMessage::Draining
        ));
        assert!(
            matches!(
                client.next().await,
                None | Some(Ok(Message::Close(_))) | Some(Err(_))
            ),
            "the server closes after Draining"
        );
        join.await.unwrap();
        assert!(!scheduler.is_worker_connected("w1").await);
    }

    /// The session handles one frame at a time, so a handler waiting on a slow
    /// actor must not look like silence to the liveness pass: the reader stamps
    /// `last_seen` on receipt, before the handler is even called.
    #[tokio::test]
    async fn the_reader_stamps_liveness_on_receipt() {
        let (socket, mut client) = connected_pair().await;
        let state = test_state(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let (reader, _writer) = socket.split(Duration::from_secs(5), &state.shutdown);
        let (session, join) = Actor::spawn(None, DecliningSession, ()).await.unwrap();
        let last_seen = Arc::new(AtomicI64::new(0));

        client
            .send(Message::Binary(
                ClientMessage::ReauthRequest.encode().unwrap(),
            ))
            .await
            .unwrap();
        read_loop(
            reader,
            session.clone(),
            Arc::clone(&last_seen),
            IN_FLIGHT_STAMP,
        )
        .await;
        session.stop(None);
        join.await.unwrap();

        assert!(
            last_seen.load(Ordering::Relaxed) > 0,
            "the reader, not the handler, stamps the frame it just received"
        );
    }

    // Regression: a gluon worker building a VM test was unregistered as dead
    // while its session waited on a saturated graph actor, heartbeats unread.
    #[tokio::test]
    async fn the_reader_keeps_stamping_while_a_frame_is_in_flight() {
        let (socket, mut client) = connected_pair().await;
        let state = test_state(MockDatabase::new(DatabaseBackend::Postgres).into_connection());
        let (reader, _writer) = socket.split(Duration::from_secs(5), &state.shutdown);
        let received = Arc::new(Notify::new());
        let (session, join) = Actor::spawn(None, HoldingSession, Arc::clone(&received))
            .await
            .unwrap();
        let last_seen = Arc::new(AtomicI64::new(0));

        client
            .send(Message::Binary(
                ClientMessage::ReauthRequest.encode().unwrap(),
            ))
            .await
            .unwrap();
        let reading = state.shutdown.spawn(read_loop(
            reader,
            session.clone(),
            Arc::clone(&last_seen),
            Duration::from_millis(20),
        ));
        received.notified().await;
        last_seen.store(0, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let stamped = last_seen.load(Ordering::Relaxed);

        session.send_message(SessionMsg::DrainDeadline).unwrap();
        reading.await.unwrap();
        session.stop(None);
        join.await.unwrap();

        assert!(
            stamped > 0,
            "a worker whose frame is still being handled is not silent"
        );
    }
}
