/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The reader is delivering one frame per call and is stamping liveness while a frame is in flight.
//! A handler waiting on a slow graph would otherwise look like a silent worker and get a healthy
//! connection unregistered.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use gradient_pool::ProjectAccess;
use gradient_pool::session_port::{SessionPort, SessionSignal};
use gradient_scheduler::Scheduler;
use gradient_storage::admission::UploadAdmission;
use gradient_types::ids::ProjectId;
use gradient_types::{RuntimeConfig, SecretString};
use gradient_util::shutdown::Shutdown;
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use sea_orm::DbErr;
use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use super::SessionHandles;
use super::auth::{check_worker_tokens, load_inbound_worker_challenge};
use super::guest_channel::outbound_guest_channel;
use super::inbound::{ActiveJobs, InboundContext, RpcContext};
use super::job_events::{JobEvents, SchedulerJobEvents};
use super::log_lane::LogLane;
use super::session_authorization::{
    HandshakeProof, SessionAuthorization, derive_session_authorization,
};
use super::socket::{
    InputFetch, ProtoSocket, ProtoWriter, send_error, send_input_fetch, send_server_msg,
};
use super::store_paths::{outbound_message, project_mapper};
use super::upload::{UploadSession, UploadTable, abandon_transfer};
use gradient_wire::Rejection;
use gradient_wire::messages::{ClientMessage, FailedPeer, GradientCapabilities, ServerMessage};
use gradient_wire::session::frame::ProtoReader;
use gradient_wire::types::JobKind;
use gradient_wire::worker_store_path_bloom_filter::WorkerStorePathBloomFilterChunkCollector;

const IN_FLIGHT_STAMP: Duration = Duration::from_secs(5);
const AUTHORIZATION_RECHECK_INTERVAL: Duration = Duration::from_secs(60);
const WORKER_SESSION_UPLOAD_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
const FAILED_AUTHORIZATION_RECHECKS_BEFORE_CLOSE: u32 = 3;

pub enum SessionMsg {
    Frame(ClientMessage, RpcReplyPort<bool>),
    Signal(SessionSignal),
    Reattach,
    ReaderClosed,
    Admitted(gradient_storage::admission::Admitted),
    SweepUploads,
    RecheckAuthorization(RpcReplyPort<bool>),
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
    pub handles: SessionHandles,
    pub scheduler: Arc<Scheduler>,
    pub socket: ProtoSocket,
    pub advertised_capabilities: GradientCapabilities,
    pub capabilities: GradientCapabilities,
    pub authorized_peers: HashSet<ProjectId>,
    pub proof: HandshakeProof,
}

pub struct SessionState {
    peer_id: String,
    handles: SessionHandles,
    scheduler: Arc<Scheduler>,
    writer: ProtoWriter,
    advertised_capabilities: GradientCapabilities,
    capabilities: GradientCapabilities,
    authorized_peers: HashSet<ProjectId>,
    proof: HandshakeProof,
    failed_authorization_rechecks: u32,
    reclaims_jobs: bool,
    uploads: UploadSession,
    nar_serve_semaphore: Arc<Semaphore>,
    worker_store_path_bloom_filter_chunk_collector: WorkerStorePathBloomFilterChunkCollector,
    active: ActiveJobs,
    job_events: JobEvents,
    logs: LogLane,
    reader: JoinHandle<()>,
    authorization_recheck_task: JoinHandle<()>,
    upload_sweep_loop_task: JoinHandle<()>,
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
            handles,
            scheduler,
            mut socket,
            advertised_capabilities,
            capabilities,
            authorized_peers,
            proof,
        } = args;
        let uploads = open_uploads(
            &handles.upload_admission,
            &handles.db.config,
            &handles.db.shutdown,
            &peer_id,
            &myself,
        )
        .map_err(|e| ActorProcessingErr::from(format!("{e:#}")))?;
        let port: Arc<dyn SessionPort> = Arc::new(SessionRef(myself.clone()));
        let reclaims_jobs = proof.dialed_session().is_none() && socket.version().is_some();
        let registered = match scheduler
            .register_worker(
                &peer_id,
                capabilities.clone(),
                ProjectAccess::OnlyListedProjects(authorized_peers.clone()),
                port,
                reclaims_jobs,
            )
            .await
        {
            Ok(registered) => registered,
            Err(e) => {
                warn!(%peer_id, error = %e, "registration failed; scheduler unavailable");
                socket.send_reject(Rejection::SchedulerUnavailable).await;
                return Err(ActorProcessingErr::from(e.to_string()));
            }
        };
        let authorization_recheck_task =
            spawn_authorization_rechecks(&handles.db.shutdown, &myself);
        let upload_sweep_loop_task =
            spawn_idle_upload_grant_expiry_loop(&handles.db.shutdown, &myself);

        let nar_cfg = &handles.db.config.nar;
        let send_chunk_timeout = Duration::from_secs(nar_cfg.send_chunk_timeout_secs.get());
        let max_serves = nar_cfg.max_concurrent_serves.get();
        let (reader, writer) = socket.split(send_chunk_timeout, None, &handles.db.shutdown);
        let tap = Arc::new(super::tap::ProtoTap {
            bus: handles.db.events.clone(),
            worker_id: peer_id.clone(),
        });
        let reader = reader.with_observer(tap.clone());
        let writer = writer.with_observer(tap);
        let active = ActiveJobs::default();
        let rpc = RpcContext::new(
            handles.clone(),
            Arc::clone(&scheduler),
            writer.clone(),
            peer_id.clone(),
            active.clone(),
        );
        let reader = handles.db.shutdown.spawn(read_loop(
            reader,
            myself,
            registered.last_seen,
            IN_FLIGHT_STAMP,
            move |msg| rpc.serve(msg),
        ));
        let job_events = JobEvents::spawn(
            &handles.db.shutdown,
            &peer_id,
            SchedulerJobEvents {
                scheduler: Arc::clone(&scheduler),
                writer: writer.clone(),
                peer_id: peer_id.clone(),
            },
        );

        let logs = LogLane::to_storage(
            &handles.db.shutdown,
            handles.db.worker_db.clone(),
            Arc::clone(&handles.db.storage.log_storage),
        );

        Ok(SessionState {
            peer_id,
            handles,
            scheduler,
            writer,
            advertised_capabilities,
            capabilities,
            authorized_peers,
            proof,
            failed_authorization_rechecks: 0,
            reclaims_jobs,
            uploads,
            nar_serve_semaphore: Arc::new(Semaphore::new(max_serves)),
            worker_store_path_bloom_filter_chunk_collector: Default::default(),
            active,
            logs,
            job_events,
            reader,
            authorization_recheck_task,
            upload_sweep_loop_task,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        st: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SessionMsg::Frame(ClientMessage::ReauthRequest, reply) => {
                let keep = answer_reauth_request(st).await;
                let _ = reply.send(keep);
                if !keep {
                    myself.stop(Some("authorization withdrawn".into()));
                }
            }
            SessionMsg::Frame(ClientMessage::AuthResponse { tokens }, reply) => {
                let keep = record_auth_response(st, tokens).await;
                let _ = reply.send(keep);
                if !keep {
                    myself.stop(Some("authorization withdrawn".into()));
                }
            }
            SessionMsg::Frame(msg, reply) => {
                let keep = {
                    let mut ctx = InboundContext {
                        writer: &st.writer,
                        handles: &st.handles,
                        scheduler: &st.scheduler,
                        peer_id: &st.peer_id,
                        nar_serve_semaphore: &st.nar_serve_semaphore,
                        active: &st.active,
                        job_events: &st.job_events,
                        logs: &st.logs,
                        dialed: st.proof.dialed_session(),
                        worker_store_path_bloom_filter_chunk_collector: &mut st
                            .worker_store_path_bloom_filter_chunk_collector,
                    };

                    ctx.handle(msg, &mut st.uploads).await
                };
                let _ = reply.send(keep);

                if !keep {
                    myself.stop(Some("peer closed".into()));
                }
            }
            SessionMsg::Admitted(admitted) => {
                let (mut ctx, uploads) = split_uploads(st);
                ctx.on_upload_admitted(admitted, uploads).await;
            }
            SessionMsg::SweepUploads => {
                let (mut ctx, uploads) = split_uploads(st);
                ctx.sweep_uploads(uploads).await;
            }
            SessionMsg::Signal(SessionSignal::JobsAvailableForRequestedKinds {
                requested_job_kinds_with_new_work,
            }) => {
                if !answer_waiting_job_requests_and_send_assignments(
                    st,
                    requested_job_kinds_with_new_work,
                )
                .await
                {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::Reauth) => {
                if !reauthorize(st).await {
                    myself.stop(Some("authorization withdrawn".into()));
                }
            }
            SessionMsg::RecheckAuthorization(reply) => {
                let keep = recheck_authorization(st).await;
                let _ = reply.send(keep);
                if !keep {
                    myself.stop(Some("authorization withdrawn".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::Abort { job_id, reason }) => {
                info!(peer_id = %st.peer_id, %job_id, %reason, "sending AbortJob to worker");
                {
                    let (mut ctx, uploads) = split_uploads(st);
                    ctx.forget_uploads(&job_id, uploads).await;
                }
                if send_server_msg(&st.writer, &ServerMessage::AbortJob { job_id, reason })
                    .await
                    .is_err()
                {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::PreWarmVms { count, sizes }) => {
                if send_server_msg(&st.writer, &ServerMessage::PreWarmVms { count, sizes })
                    .await
                    .is_err()
                {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::ImportResult {
                job_id,
                request_id,
                outcome,
            }) => {
                let Some(project) = st.active.project(&job_id) else {
                    debug!(peer_id = %st.peer_id, %job_id, "import result for a job this session no longer runs");
                    return Ok(());
                };

                let msg = ServerMessage::ImportResult {
                    job_id,
                    request_id,
                    outcome,
                };
                let Some(msg) = outbound_to(st, project, msg).await else {
                    return Ok(());
                };
                if send_server_msg(&st.writer, &msg).await.is_err() {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::InputResult {
                job_id,
                request_id,
                outcome,
            }) => {
                let Some(project) = st.active.project(&job_id) else {
                    debug!(peer_id = %st.peer_id, %job_id, "input result for a job this session no longer runs");
                    return Ok(());
                };

                let msg = ServerMessage::InputResult {
                    job_id,
                    request_id,
                    outcome,
                };
                let Some(msg) = outbound_to(st, project, msg).await else {
                    return Ok(());
                };
                if send_server_msg(&st.writer, &msg).await.is_err() {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::InputProgress {
                job_id,
                request_id,
                bytes_done,
                bytes_total,
            }) => {
                if !st.active.contains(&job_id) {
                    return Ok(());
                }

                let msg = ServerMessage::InputProgress {
                    job_id,
                    request_id,
                    bytes_done,
                    bytes_total,
                };
                if send_server_msg(&st.writer, &msg).await.is_err() {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::FetchInput {
                fetch_id,
                project,
                request,
                needs_secrets,
            }) => {
                st.active.insert_fetch(fetch_id.clone(), project);
                let fetch = InputFetch {
                    fetch_id,
                    project,
                    request,
                    needs_secrets,
                };
                if send_input_fetch(
                    &st.writer,
                    &st.handles.db,
                    &st.scheduler,
                    &st.peer_id,
                    fetch,
                )
                .await
                .is_err()
                {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::StartCluster { attempt, roster }) => {
                let msg = ServerMessage::StartCluster { attempt, roster };
                if send_server_msg(&st.writer, &msg).await.is_err() {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::ClusterSignal {
                attempt,
                from,
                payload,
            }) => {
                let msg = ServerMessage::ClusterSignal {
                    attempt,
                    from,
                    payload: payload.into(),
                };
                if send_server_msg(&st.writer, &msg).await.is_err() {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::AbortCluster { attempt, reason }) => {
                info!(peer_id = %st.peer_id, %attempt, %reason, "sending AbortCluster to worker");
                if let Ok(id) = attempt.parse() {
                    let members = st.active.remove_attempt(id);
                    let (mut ctx, uploads) = split_uploads(st);
                    for job_id in &members {
                        ctx.forget_uploads(job_id, uploads).await;
                    }
                }
                let msg = ServerMessage::AbortCluster { attempt, reason };
                if send_server_msg(&st.writer, &msg).await.is_err() {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::GuestChannel(signal)) => {
                let msg = outbound_guest_channel(signal);
                if send_server_msg(&st.writer, &msg).await.is_err() {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::Draining) => {
                info!(peer_id = %st.peer_id, "sending Draining to worker");
                if send_server_msg(&st.writer, &ServerMessage::Draining)
                    .await
                    .is_err()
                {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::DrainCancelled) => {
                info!(peer_id = %st.peer_id, "sending DrainCancelled to worker");
                if send_server_msg(&st.writer, &ServerMessage::DrainCancelled)
                    .await
                    .is_err()
                {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::ExecKeepVmRunning {
                job_id,
                keep_vm_running,
            }) => {
                let msg = ServerMessage::ExecKeepVmRunning {
                    job_id,
                    keep_vm_running,
                };
                if send_server_msg(&st.writer, &msg).await.is_err() {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::ClusterAssign { job_id }) => {
                let Some(prepared) = st.scheduler.cluster.prepared.take(&job_id).await else {
                    return Ok(());
                };
                let (mut ctx, uploads) = split_uploads(st);
                if !ctx
                    .hand_out(prepared.assignment, Some(prepared.membership), uploads)
                    .await
                {
                    myself.stop(Some("write failed".into()));
                }
            }
            SessionMsg::Signal(SessionSignal::Close { reason }) => {
                warn!(peer_id = %st.peer_id, %reason, "closing session at the scheduler's request");
                myself.stop(Some(reason));
            }
            SessionMsg::Reattach => {
                let port: Arc<dyn SessionPort> = Arc::new(SessionRef(myself.clone()));
                let active = st.active.pending();
                if let Err(e) = st
                    .scheduler
                    .reattach_worker(
                        &st.peer_id,
                        st.capabilities.clone(),
                        ProjectAccess::OnlyListedProjects(st.authorized_peers.clone()),
                        port,
                        active,
                        st.reclaims_jobs,
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
        st.authorization_recheck_task.abort();
        st.upload_sweep_loop_task.abort();
        for granted in st.uploads.table.drain_granted() {
            let nar_storage = st.handles.db.storage.nar_storage.clone();
            st.handles
                .db
                .shutdown
                .spawn(async move { abandon_transfer(&nar_storage, granted).await });
        }
        st.job_events.finish().await;
        st.scheduler.guest_channels.close_worker(&st.peer_id);
        st.scheduler.unregister_worker(&st.peer_id).await;
        info!(peer_id = %st.peer_id, "WebSocket connection closed");
        Ok(())
    }
}

async fn outbound_to(
    st: &SessionState,
    project: ProjectId,
    msg: ServerMessage,
) -> Option<ServerMessage> {
    let mapper = project_mapper(&st.handles.db, project).await?;
    Some(outbound_message(msg, &mapper))
}

async fn answer_waiting_job_requests_and_send_assignments(
    st: &mut SessionState,
    job_kinds: Vec<JobKind>,
) -> bool {
    let (mut ctx, uploads) = split_uploads(st);
    for kind in job_kinds {
        if !ctx
            .send_assign_job_for_available_requested_job_kind(kind, uploads)
            .await
        {
            return false;
        }
    }
    true
}

/// Unanswered frames are budgeted in bytes, not in count: a graph that lags behind holds up many
/// small reports and a frame cap stopped the reader before a lookup the worker awaits.
const READ_AHEAD_BYTES: usize = 32 * 1024 * 1024;

async fn read_loop(
    mut reader: ProtoReader,
    session: ActorRef<SessionMsg>,
    last_seen: Arc<AtomicI64>,
    stamp_every: Duration,
    serve: impl Fn(ClientMessage) -> Option<ClientMessage> + Send + 'static,
) {
    let stamp = || {
        last_seen.store(
            gradient_types::now().and_utc().timestamp_millis(),
            Ordering::Relaxed,
        )
    };
    let mut unanswered: VecDeque<(oneshot::Receiver<bool>, usize)> = VecDeque::new();
    let mut unanswered_bytes = 0;
    let mut in_flight =
        tokio::time::interval_at(tokio::time::Instant::now() + stamp_every, stamp_every);
    loop {
        tokio::select! {
            biased;
            answered = async { (&mut unanswered.front_mut().expect("guarded").0).await }, if !unanswered.is_empty() => {
                if let Some((_, bytes)) = unanswered.pop_front() {
                    unanswered_bytes -= bytes;
                }
                if !matches!(answered, Ok(true)) {
                    return;
                }
            }
            inbound = reader.recv_sized(), if unanswered_bytes < READ_AHEAD_BYTES => {
                let Some((inbound, len)) = inbound else { break };
                stamp();
                let Some(msg) = serve(inbound) else { continue };
                let (reply, answered) = oneshot::channel();
                if session.send_message(SessionMsg::Frame(msg, reply.into())).is_err() {
                    return;
                }
                let bytes = len + size_of::<SessionMsg>();
                unanswered_bytes += bytes;
                unanswered.push_back((answered, bytes));
            }
            _ = in_flight.tick(), if !unanswered.is_empty() => stamp(),
        }
    }

    let _ = session.send_message(SessionMsg::ReaderClosed);
}

fn open_uploads(
    upload_admission: &Arc<UploadAdmission>,
    config: &RuntimeConfig,
    shutdown: &Shutdown,
    peer_id: &str,
    myself: &ActorRef<SessionMsg>,
) -> anyhow::Result<UploadSession> {
    let (admission, mut admitted) = upload_admission.open_session(peer_id);
    let uploads = UploadSession {
        admission,
        table: UploadTable::default(),
        partials: gradient_storage::PartialStore::new(config.server.nar_partial_dir())?,
        retain_up_to: config.nar.small_bytes,
        idle_lease: Duration::from_secs(config.upload.lease_idle_secs.get()),
    };
    let forward = myself.clone();
    shutdown.spawn(async move {
        while let Some(a) = admitted.recv().await {
            if forward.send_message(SessionMsg::Admitted(a)).is_err() {
                break;
            }
        }
    });
    Ok(uploads)
}

fn spawn_idle_upload_grant_expiry_loop(
    shutdown: &Shutdown,
    myself: &ActorRef<SessionMsg>,
) -> JoinHandle<()> {
    spawn_tracked_worker_session_interval_loop(
        shutdown,
        myself,
        WORKER_SESSION_UPLOAD_SWEEP_INTERVAL,
        |session| async move { session.send_message(SessionMsg::SweepUploads).is_ok() },
    )
}

fn spawn_authorization_rechecks(
    shutdown: &Shutdown,
    myself: &ActorRef<SessionMsg>,
) -> JoinHandle<()> {
    spawn_tracked_worker_session_interval_loop(
        shutdown,
        myself,
        AUTHORIZATION_RECHECK_INTERVAL,
        |session| async move {
            let recheck = session.call(SessionMsg::RecheckAuthorization, None).await;
            matches!(recheck, Ok(ractor::rpc::CallResult::Success(true)))
        },
    )
}

fn spawn_tracked_worker_session_interval_loop<Step, StepFuture>(
    shutdown: &Shutdown,
    myself: &ActorRef<SessionMsg>,
    interval: Duration,
    step: Step,
) -> JoinHandle<()>
where
    Step: Fn(ActorRef<SessionMsg>) -> StepFuture + Send + 'static,
    StepFuture: Future<Output = bool> + Send,
{
    let session = myself.clone();
    let server_shutdown_cancel_token = shutdown.token();
    shutdown.spawn(async move {
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                () = server_shutdown_cancel_token.cancelled() => break,
                _ = tick.tick() => {}
            }
            if !step(session.clone()).await {
                break;
            }
        }
    })
}

async fn reauthorize(st: &mut SessionState) -> bool {
    reauthorize_reporting_failed_tokens(st, Vec::new()).await
}

async fn recheck_authorization(st: &mut SessionState) -> bool {
    let decision = derive_session_authorization(
        &st.handles.db.worker_db,
        &st.handles.db.config,
        &st.peer_id,
        &st.proof,
        st.advertised_capabilities.clone(),
    )
    .await;
    if decision.is_err() {
        st.failed_authorization_rechecks += 1;
        if st.failed_authorization_rechecks >= FAILED_AUTHORIZATION_RECHECKS_BEFORE_CLOSE {
            warn!(peer_id = %st.peer_id, failed = st.failed_authorization_rechecks, "The session's authorization could not be read for several intervals in a row. Closing the session.");
            return false;
        }
    }
    apply_session_authorization(st, decision, AuthUpdateRule::OnlyWhenProjectsChange).await
}

async fn reauthorize_reporting_failed_tokens(
    st: &mut SessionState,
    failed_tokens: Vec<FailedPeer>,
) -> bool {
    let decision = derive_session_authorization(
        &st.handles.db.worker_db,
        &st.handles.db.config,
        &st.peer_id,
        &st.proof,
        st.advertised_capabilities.clone(),
    )
    .await
    .map(|decision| decision.including_failed_peers(failed_tokens));
    apply_session_authorization(st, decision, AuthUpdateRule::Always).await
}

async fn answer_reauth_request(st: &mut SessionState) -> bool {
    debug!(peer_id = %st.peer_id, "ReauthRequest");
    if !reauthorize(st).await {
        return false;
    }
    if st.proof.dialed_session().is_some() {
        return true;
    }
    let registered = match load_inbound_worker_challenge(&st.handles.db.worker_db, &st.peer_id)
        .await
    {
        Ok((_, registered)) => registered,
        Err(e) => {
            warn!(peer_id = %st.peer_id, error = %e, "Failed to read the worker's registrations for the challenge. The session stays open and no challenge is sent.");
            return true;
        }
    };
    send_server_msg(
        &st.writer,
        &ServerMessage::AuthChallenge {
            peers: registered.into_iter().map(|(peer, _)| peer).collect(),
        },
    )
    .await
    .is_ok()
}

async fn record_auth_response(st: &mut SessionState, tokens: Vec<(String, SecretString)>) -> bool {
    if st.proof.dialed_session().is_some() {
        warn!(peer_id = %st.peer_id, variant = "AuthResponse", "A message that only a worker-connected session can send reached a server-dialed session. Close the session.");
        send_error(
            &st.writer,
            400,
            "Unexpected AuthResponse in server-dialed session".into(),
        )
        .await;
        return false;
    }
    let db = &st.handles.db.worker_db;
    let checked = match load_inbound_worker_challenge(db, &st.peer_id).await {
        Ok((_, registered)) => check_worker_tokens(&registered, &tokens).await,
        Err(e) => Err(e.into()),
    };
    let checked = match checked {
        Ok(checked) => checked,
        Err(e) => {
            warn!(peer_id = %st.peer_id, error = %e, "Failed to look up data for re-authentication. Closing connection.");
            return false;
        }
    };
    st.proof = HandshakeProof::WorkerDialed {
        proven_peer_token_hashes: checked.proven_peer_token_hashes,
    };
    reauthorize_reporting_failed_tokens(st, checked.failed_peers).await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthUpdateRule {
    Always,
    OnlyWhenProjectsChange,
}

async fn apply_session_authorization(
    st: &mut SessionState,
    decision: Result<SessionAuthorization, DbErr>,
    rule: AuthUpdateRule,
) -> bool {
    if decision.is_ok() {
        st.failed_authorization_rechecks = 0;
    }
    match decision {
        Ok(SessionAuthorization::Authorized {
            projects,
            failed_peers,
            capabilities,
        }) => {
            st.scheduler
                .replace_worker_authorization(
                    &st.peer_id,
                    ProjectAccess::OnlyListedProjects(projects.clone()),
                    capabilities.clone(),
                )
                .await;
            let projects_changed = projects != st.authorized_peers;
            let authorized_peers = projects.iter().map(ToString::to_string).collect();
            st.authorized_peers = projects;
            st.capabilities = capabilities;
            if rule == AuthUpdateRule::OnlyWhenProjectsChange && !projects_changed {
                return true;
            }
            send_server_msg(
                &st.writer,
                &ServerMessage::AuthUpdate {
                    authorized_peers,
                    failed_peers,
                },
            )
            .await
            .is_ok()
        }
        Ok(SessionAuthorization::RedialForUncoveredProject { project }) => {
            info!(peer_id = %st.peer_id, %project, "project not covered by this session's tokens - closing for a redial");
            false
        }
        Ok(SessionAuthorization::Rejected(rejection)) => {
            info!(peer_id = %st.peer_id, code = rejection.code(), reason = %rejection, "worker is no longer authorized - disconnecting");
            st.scheduler
                .replace_worker_authorization(
                    &st.peer_id,
                    ProjectAccess::no_project_access(),
                    GradientCapabilities::default(),
                )
                .await;
            let _ = send_server_msg(&st.writer, &ServerMessage::from(rejection)).await;
            false
        }
        Err(e) => {
            warn!(peer_id = %st.peer_id, error = %e, "failed to read the session's authorization; keeping the current one");
            true
        }
    }
}

fn split_uploads(st: &mut SessionState) -> (InboundContext<'_>, &mut UploadSession) {
    (
        InboundContext {
            writer: &st.writer,
            handles: &st.handles,
            scheduler: &st.scheduler,
            peer_id: &st.peer_id,
            nar_serve_semaphore: &st.nar_serve_semaphore,
            active: &st.active,
            job_events: &st.job_events,
            logs: &st.logs,
            dialed: st.proof.dialed_session(),
            worker_store_path_bloom_filter_chunk_collector: &mut st
                .worker_store_path_bloom_filter_chunk_collector,
        },
        &mut st.uploads,
    )
}

#[cfg(test)]
pub(in crate::handler) mod tests {
    use super::super::sessions::{Sessions, SessionsArgs, SessionsMsg};
    use super::*;
    use bytes::Bytes;
    use futures::{SinkExt, StreamExt};
    use gradient_fixtures::mock::empty_db;
    use gradient_test_support::prelude::*;
    use gradient_wire::PROTO_VERSIONS;
    use gradient_wire::codec::{from_bytes, to_bytes};
    use ractor::rpc::CallResult;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::Notify;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

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
                SessionMsg::Reattach => {
                    if let Some(reply) = held.take() {
                        let _ = reply.send(false);
                    }
                }
                _ => {}
            }

            Ok(())
        }
    }

    struct BacklogSession;

    impl Actor for BacklogSession {
        type Msg = SessionMsg;
        type State = tokio::sync::watch::Receiver<bool>;
        type Arguments = tokio::sync::watch::Receiver<bool>;

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            graph_caught_up: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(graph_caught_up)
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            msg: Self::Msg,
            graph_caught_up: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let SessionMsg::Frame(_, reply) = msg {
                let _ = graph_caught_up.wait_for(|caught_up| *caught_up).await;
                let _ = reply.send(false);
            }

            Ok(())
        }
    }

    struct SweepUploadsCountingSession;

    impl Actor for SweepUploadsCountingSession {
        type Msg = SessionMsg;
        type State = Arc<std::sync::atomic::AtomicUsize>;
        type Arguments = (SessionHandles, Arc<std::sync::atomic::AtomicUsize>);

        async fn pre_start(
            &self,
            myself: ActorRef<Self::Msg>,
            (handles, received_sweep_uploads_message_count): Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            spawn_idle_upload_grant_expiry_loop(&handles.db.shutdown, &myself);
            Ok(received_sweep_uploads_message_count)
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            msg: Self::Msg,
            received_sweep_uploads_message_count: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if matches!(msg, SessionMsg::SweepUploads) {
                received_sweep_uploads_message_count.fetch_add(1, Ordering::Relaxed);
            }

            Ok(())
        }
    }

    async fn spawn_sweep_uploads_counting_session_test_actor(
        state: &Arc<gradient_core::ServerState>,
    ) -> (
        ActorRef<SessionMsg>,
        ractor::concurrency::JoinHandle<()>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let received_sweep_uploads_message_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (session, join) = Actor::spawn(
            None,
            SweepUploadsCountingSession,
            (
                SessionHandles::from_state(state),
                Arc::clone(&received_sweep_uploads_message_count),
            ),
        )
        .await
        .unwrap();
        (session, join, received_sweep_uploads_message_count)
    }

    #[tokio::test(start_paused = true)]
    async fn upload_sweep_loop_started_during_pre_start_sends_sweep_uploads_at_least_twice_after_two_sweep_intervals()
     {
        let state = test_state(empty_db());
        let (session, join, received_sweep_uploads_message_count) =
            spawn_sweep_uploads_counting_session_test_actor(&state).await;

        tokio::time::sleep(WORKER_SESSION_UPLOAD_SWEEP_INTERVAL * 2 + Duration::from_secs(1)).await;
        let upload_sweep_count_after_clock_advance =
            received_sweep_uploads_message_count.load(Ordering::Relaxed);

        session.stop(None);
        join.await.unwrap();
        assert!(
            upload_sweep_count_after_clock_advance >= 2,
            "The upload sweep ran fewer than 2 times after 2 sweep intervals. ran {upload_sweep_count_after_clock_advance} times"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_and_drain_with_a_short_budget_returns_true_while_session_upload_sweep_loop_is_open_and_no_upload_sweep_runs_after_server_shutdown_token_cancellation()
     {
        let state = test_state(empty_db());
        let (session, join, received_sweep_uploads_message_count) =
            spawn_sweep_uploads_counting_session_test_actor(&state).await;

        let drained_all_tasks_in_time = state
            .shutdown
            .cancel_and_drain(Duration::from_millis(200))
            .await;
        let upload_sweeps_after_shutdown =
            received_sweep_uploads_message_count.load(Ordering::Relaxed);
        tokio::time::sleep(WORKER_SESSION_UPLOAD_SWEEP_INTERVAL * 3).await;
        let upload_sweeps_three_intervals_after_shutdown_cancelled =
            received_sweep_uploads_message_count.load(Ordering::Relaxed);

        session.stop(None);
        join.await.unwrap();
        assert!(
            drained_all_tasks_in_time,
            "The loop that ran upload sweeps blocked tracked tasks from draining after cancellation of the shutdown token."
        );
        assert_eq!(
            upload_sweeps_three_intervals_after_shutdown_cancelled, upload_sweeps_after_shutdown,
            "upload sweep ran after cancellation of the shutdown token"
        );
    }

    pub(in crate::handler) async fn connected_pair()
    -> (ProtoSocket, WebSocketStream<MaybeTlsStream<TcpStream>>) {
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
            ProtoSocket::tungstenite(server).with_version(*PROTO_VERSIONS.end()),
            client.unwrap().0,
        )
    }

    #[tokio::test]
    async fn an_abort_cluster_signal_reaches_the_worker() {
        let (socket, mut client) = connected_pair().await;
        let state = test_state(empty_db());
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
        scheduler.spawn_core(None).await.unwrap();
        let (actor, _join) = Actor::spawn(
            None,
            SessionActor,
            SessionArgs {
                peer_id: "w1".into(),
                handles: SessionHandles::from_state(&state),
                scheduler: Arc::clone(&scheduler),
                socket,
                advertised_capabilities: GradientCapabilities::default(),
                capabilities: GradientCapabilities::default(),
                authorized_peers: HashSet::new(),
                proof: HandshakeProof::WorkerDialed {
                    proven_peer_token_hashes: Vec::new(),
                },
            },
        )
        .await
        .unwrap();

        actor
            .send_message(SessionMsg::Signal(SessionSignal::AbortCluster {
                attempt: "a1".into(),
                reason: "member lost".into(),
            }))
            .unwrap();

        let frame = client
            .next()
            .await
            .expect("a frame")
            .expect("no transport error");
        let Message::Binary(bytes) = frame else {
            panic!("expected a binary frame, got {frame:?}");
        };
        assert_eq!(
            from_bytes::<ServerMessage>(bytes, *PROTO_VERSIONS.end()).unwrap(),
            ServerMessage::AbortCluster {
                attempt: "a1".into(),
                reason: "member lost".into(),
            }
        );
    }

    #[tokio::test]
    async fn pre_warm_vms_reaches_the_worker() {
        let (socket, mut client) = connected_pair().await;
        let state = test_state(empty_db());
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
        scheduler.spawn_core(None).await.unwrap();
        let (actor, _join) = Actor::spawn(
            None,
            SessionActor,
            SessionArgs {
                peer_id: "w1".into(),
                handles: SessionHandles::from_state(&state),
                scheduler: Arc::clone(&scheduler),
                socket,
                advertised_capabilities: GradientCapabilities::default(),
                capabilities: GradientCapabilities::default(),
                authorized_peers: HashSet::new(),
                proof: HandshakeProof::WorkerDialed {
                    proven_peer_token_hashes: Vec::new(),
                },
            },
        )
        .await
        .unwrap();

        actor
            .send_message(SessionMsg::Signal(SessionSignal::PreWarmVms {
                count: 2,
                sizes: Vec::new(),
            }))
            .unwrap();

        let frame = client
            .next()
            .await
            .expect("a frame")
            .expect("no transport error");
        let Message::Binary(bytes) = frame else {
            panic!("expected a binary frame, got {frame:?}");
        };
        assert_eq!(
            from_bytes::<ServerMessage>(bytes, *PROTO_VERSIONS.end()).unwrap(),
            ServerMessage::PreWarmVms {
                count: 2,
                sizes: Vec::new()
            }
        );
    }

    #[tokio::test]
    async fn stopping_the_sessions_closes_every_socket_without_a_draining_frame() {
        let (socket, mut client) = connected_pair().await;
        let state = test_state(empty_db());
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
        scheduler.spawn_core(None).await.unwrap();
        let (sessions, sessions_join) = Actor::spawn(
            None,
            Sessions,
            SessionsArgs {
                scheduler: Arc::clone(&scheduler),
                shutdown: state.shutdown.clone(),
            },
        )
        .await
        .unwrap();
        let args = SessionArgs {
            peer_id: "w1".into(),
            handles: SessionHandles::from_state(&state),
            scheduler: Arc::clone(&scheduler),
            socket,
            advertised_capabilities: GradientCapabilities::default(),
            capabilities: GradientCapabilities::default(),
            authorized_peers: HashSet::new(),
            proof: HandshakeProof::WorkerDialed {
                proven_peer_token_hashes: Vec::new(),
            },
        };
        let CallResult::Success(Ok((_session, session_join))) = sessions
            .call(|reply| SessionsMsg::Attach(Box::new(args), reply), None)
            .await
            .unwrap()
        else {
            panic!("the session attaches");
        };
        assert!(scheduler.is_worker_connected("w1").await);

        sessions.stop(None);
        sessions_join.await.unwrap();
        session_join.await.unwrap();

        let frames = tokio::time::timeout(Duration::from_secs(5), async {
            let mut frames = Vec::new();
            while let Some(Ok(Message::Binary(bytes))) = client.next().await {
                frames.push(from_bytes::<ServerMessage>(bytes, *PROTO_VERSIONS.end()).unwrap());
            }
            frames
        })
        .await
        .expect("the server closes the socket");
        assert!(!frames.contains(&ServerMessage::Draining), "{frames:?}");
        assert!(!scheduler.is_worker_connected("w1").await);
    }

    #[tokio::test]
    async fn cancel_and_drain_with_a_short_budget_returns_true_while_a_session_authorization_recheck_task_is_open()
     {
        let shutdown = Shutdown::new();
        let (session, join) = Actor::spawn(None, DecliningSession, ()).await.unwrap();
        spawn_authorization_rechecks(&shutdown, &session);

        let drained_all_tasks_in_time = shutdown.cancel_and_drain(Duration::from_millis(200)).await;

        session.stop(None);
        join.await.unwrap();
        assert!(drained_all_tasks_in_time);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_shutdown_token_when_interval_tick_is_due_never_runs_the_tracked_worker_session_step_repeated_two_hundred_times()
     {
        let (session, join) = Actor::spawn(None, DecliningSession, ()).await.unwrap();
        let interval_loop_step_run_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        for _ in 0..200 {
            let shutdown = Shutdown::new();
            let server_shutdown_cancel_token = shutdown.token();
            let interval_loop_step_run_count = Arc::clone(&interval_loop_step_run_count);
            spawn_tracked_worker_session_interval_loop(
                &shutdown,
                &session,
                AUTHORIZATION_RECHECK_INTERVAL,
                move |_| {
                    let server_shutdown_cancel_token = server_shutdown_cancel_token.clone();
                    let interval_loop_step_run_count = Arc::clone(&interval_loop_step_run_count);
                    async move {
                        interval_loop_step_run_count.fetch_add(1, Ordering::Relaxed);
                        tokio::time::sleep(AUTHORIZATION_RECHECK_INTERVAL).await;
                        server_shutdown_cancel_token.cancel();
                        true
                    }
                },
            )
            .await
            .unwrap();
        }

        session.stop(None);
        join.await.unwrap();
        let interval_loop_step_run_count = interval_loop_step_run_count.load(Ordering::Relaxed);
        assert_eq!(
            interval_loop_step_run_count, 200,
            "A step of the interval loop ran even though cancellation of the shutdown token came at the same time as the tick. Run count: {interval_loop_step_run_count}"
        );
    }

    #[tokio::test]
    async fn stopping_worker_session_actor_ends_tracked_tasks_without_server_shutdown() {
        let (socket, _client) = connected_pair().await;
        let state = test_state(empty_db());
        let second_server_state = test_state(empty_db());
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
        scheduler.spawn_core(None).await.unwrap();
        let (actor, join) = Actor::spawn(
            None,
            SessionActor,
            SessionArgs {
                peer_id: "w1".into(),
                handles: SessionHandles::from_state(&second_server_state),
                scheduler: Arc::clone(&scheduler),
                socket,
                advertised_capabilities: GradientCapabilities::default(),
                capabilities: GradientCapabilities::default(),
                authorized_peers: HashSet::new(),
                proof: HandshakeProof::WorkerDialed {
                    proven_peer_token_hashes: Vec::new(),
                },
            },
        )
        .await
        .unwrap();

        actor.stop(None);
        join.await.unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            while second_server_state.shutdown.pending() > 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!second_server_state.shutdown.is_cancelled());
    }

    #[tokio::test]
    async fn the_reader_stamps_liveness_on_receipt() {
        let (socket, mut client) = connected_pair().await;
        let state = test_state(empty_db());
        let (reader, _writer) = socket.split(Duration::from_secs(5), None, &state.shutdown);
        let (session, join) = Actor::spawn(None, DecliningSession, ()).await.unwrap();
        let last_seen = Arc::new(AtomicI64::new(0));

        client
            .send(Message::Binary(
                to_bytes(&ClientMessage::ReauthRequest, *PROTO_VERSIONS.end()).unwrap(),
            ))
            .await
            .unwrap();
        read_loop(
            reader,
            session.clone(),
            Arc::clone(&last_seen),
            IN_FLIGHT_STAMP,
            Some,
        )
        .await;
        session.stop(None);
        join.await.unwrap();

        assert!(
            last_seen.load(Ordering::Relaxed) > 0,
            "the reader, not the handler, stamps the frame it just received"
        );
    }

    #[tokio::test]
    async fn the_reader_keeps_stamping_while_a_frame_is_in_flight() {
        let (socket, mut client) = connected_pair().await;
        let state = test_state(empty_db());
        let (reader, _writer) = socket.split(Duration::from_secs(5), None, &state.shutdown);
        let received = Arc::new(Notify::new());
        let (session, join) = Actor::spawn(None, HoldingSession, Arc::clone(&received))
            .await
            .unwrap();
        let last_seen = Arc::new(AtomicI64::new(0));

        client
            .send(Message::Binary(
                to_bytes(&ClientMessage::ReauthRequest, *PROTO_VERSIONS.end()).unwrap(),
            ))
            .await
            .unwrap();
        let reading = state.shutdown.spawn(read_loop(
            reader,
            session.clone(),
            Arc::clone(&last_seen),
            Duration::from_millis(20),
            Some,
        ));
        received.notified().await;
        last_seen.store(0, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let stamped = last_seen.load(Ordering::Relaxed);

        session.send_message(SessionMsg::Reattach).unwrap();
        reading.await.unwrap();
        session.stop(None);
        join.await.unwrap();

        assert!(
            stamped > 0,
            "a worker whose frame is still being handled is not silent"
        );
    }

    #[tokio::test]
    async fn a_cache_query_is_served_while_an_earlier_frame_is_held() {
        let (socket, mut client) = connected_pair().await;
        let state = test_state(empty_db());
        let (reader, _writer) = socket.split(Duration::from_secs(5), None, &state.shutdown);
        let received = Arc::new(Notify::new());
        let (session, join) = Actor::spawn(None, HoldingSession, Arc::clone(&received))
            .await
            .unwrap();
        let (served_tx, mut served) = tokio::sync::mpsc::unbounded_channel();

        for msg in [
            ClientMessage::ReauthRequest,
            ClientMessage::CacheQuery {
                job_id: "j1".into(),
                query_id: "q1".into(),
                paths: Vec::new(),
                nar_sizes: Vec::new(),
                mode: gradient_wire::types::QueryMode::Pull,
                external: false,
            },
        ] {
            client
                .send(Message::Binary(
                    to_bytes(&msg, *PROTO_VERSIONS.end()).unwrap(),
                ))
                .await
                .unwrap();
        }
        let reading = state.shutdown.spawn(read_loop(
            reader,
            session.clone(),
            Arc::new(AtomicI64::new(0)),
            IN_FLIGHT_STAMP,
            move |msg| match msg {
                ClientMessage::CacheQuery { query_id, .. } => {
                    let _ = served_tx.send(query_id);
                    None
                }
                other => Some(other),
            },
        ));
        received.notified().await;
        let query = tokio::time::timeout(Duration::from_secs(5), served.recv())
            .await
            .expect("the lookup is not queued behind the held frame");

        session.send_message(SessionMsg::Reattach).unwrap();
        reading.await.unwrap();
        session.stop(None);
        join.await.unwrap();

        assert_eq!(query.as_deref(), Some("q1"));
    }

    #[tokio::test]
    async fn a_cache_query_behind_hundreds_of_unanswered_log_chunks_is_served() {
        let (socket, mut client) = connected_pair().await;
        let state = test_state(empty_db());
        let (reader, _writer) = socket.split(Duration::from_secs(5), None, &state.shutdown);
        let (catch_up, graph_caught_up) = tokio::sync::watch::channel(false);
        let (session, join) = Actor::spawn(None, BacklogSession, graph_caught_up)
            .await
            .unwrap();
        let (served_tx, mut served) = tokio::sync::mpsc::unbounded_channel();

        let log_chunks = (0..500).map(|_| ClientMessage::LogChunk {
            job_id: "j1".into(),
            data: Bytes::from_static(b"building\n"),
        });
        let query = ClientMessage::CacheQuery {
            job_id: "j1".into(),
            query_id: "q1".into(),
            paths: Vec::new(),
            nar_sizes: Vec::new(),
            mode: gradient_wire::types::QueryMode::Pull,
            external: false,
        };
        for msg in log_chunks.chain([query]) {
            client
                .feed(Message::Binary(
                    to_bytes(&msg, *PROTO_VERSIONS.end()).unwrap(),
                ))
                .await
                .unwrap();
        }
        client.flush().await.unwrap();
        let reading = state.shutdown.spawn(read_loop(
            reader,
            session.clone(),
            Arc::new(AtomicI64::new(0)),
            IN_FLIGHT_STAMP,
            move |msg| match msg {
                ClientMessage::CacheQuery { query_id, .. } => {
                    let _ = served_tx.send(query_id);
                    None
                }
                other => Some(other),
            },
        ));
        let query = tokio::time::timeout(Duration::from_secs(5), served.recv())
            .await
            .expect("the lookup is read while the session is behind the graph");

        catch_up.send(true).unwrap();
        reading.await.unwrap();
        session.stop(None);
        join.await.unwrap();

        assert_eq!(query.as_deref(), Some("q1"));
    }
}
