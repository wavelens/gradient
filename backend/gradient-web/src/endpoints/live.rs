/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, ProjectItemAccessRequirement, TaskAccess, load_task};
use crate::api_spec::TaskPath;
use crate::authorization::{
    MaybeApiKey, MaybeUser, WebSocketUpgradeCredentialRecheckRequestExtractor,
};
use crate::error::{WebError, WebResult};
use crate::extractors::Path;
use crate::scope::{Readable, TaskScope};
use axum::Extension;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, close_code};
use axum::http::Method;
use axum::response::Response;
use bytes::Bytes;
use futures::{Sink, Stream};
use gradient_core::ServerState;
use gradient_types::events::{Envelope, Event, EventRx, evaluation};
use gradient_types::*;
use gradient_util::shutdown::CancellationToken;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{Instant, Interval, MissedTickBehavior};

use super::builds::BuildAccessContext;
use super::evals::EvalAccessContext;

pub const LIVE_STREAM_PING_INTERVAL: Duration = Duration::from_secs(30);
pub const LIVE_STREAM_CLIENT_RECEIVE_TIMEOUT: Duration = Duration::from_secs(60);
/// 30 seconds. This is the default timeout for sending each chunk of the cache protocol
/// (`nar send_chunk_timeout_secs`). One limit is the bound for every send to a peer that stopped
/// reading.
pub const LIVE_STREAM_OR_JOB_TERMINAL_BROWSER_MESSAGE_SEND_TIMEOUT: Duration =
    Duration::from_secs(30);

pub(crate) async fn send_websocket_message_to_browser_with_timeout<S>(
    sink: &mut S,
    message: Message,
) -> bool
where
    S: Sink<Message> + Unpin,
{
    use futures::SinkExt;

    matches!(
        tokio::time::timeout(
            LIVE_STREAM_OR_JOB_TERMINAL_BROWSER_MESSAGE_SEND_TIMEOUT,
            sink.send(message),
        )
        .await,
        Ok(Ok(()))
    )
}

pub(crate) struct ClientPingTimerAndLastReceivedTime {
    ping: Interval,
    last_received: Instant,
}

impl ClientPingTimerAndLastReceivedTime {
    pub(crate) fn new() -> Self {
        let now = Instant::now();
        let mut ping =
            tokio::time::interval_at(now + LIVE_STREAM_PING_INTERVAL, LIVE_STREAM_PING_INTERVAL);
        ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Self {
            ping,
            last_received: now,
        }
    }

    pub(crate) fn reset_silence_timer_after_client_frame(&mut self) {
        self.last_received = Instant::now();
    }

    pub(crate) async fn wait_for_next_ping_or_silence_deadline(&mut self) -> Option<Message> {
        let deadline = self.last_received + LIVE_STREAM_CLIENT_RECEIVE_TIMEOUT;
        tokio::select! {
            biased;
            () = tokio::time::sleep_until(deadline) => None,
            _ = self.ping.tick() => Some(Message::Ping(Bytes::new())),
        }
    }
}

/// The receive side reads the socket to notice that the client left and to take the Pong. The loop
/// sends a Ping every 30 seconds. The stream ends after 60 seconds without anything from the
/// client, which frees the socket of a client that vanished without closing it.
pub async fn live_stream<S, E, F, L>(
    socket: S,
    mut rx: EventRx,
    mut select: F,
    lagged: L,
    access_lost: impl Future<Output = ()> + Send,
    cancel: CancellationToken,
) where
    S: Stream<Item = Result<Message, E>> + Sink<Message> + Send,
    E: Send,
    F: FnMut(&Envelope) -> Option<String> + Send + 'static,
    L: Fn(u64) -> Option<String> + Send + 'static,
{
    use futures::StreamExt;
    use tokio::sync::broadcast::error::RecvError;

    let (mut sink, mut stream) = socket.split();
    let mut liveness = ClientPingTimerAndLastReceivedTime::new();
    tokio::pin!(access_lost);
    loop {
        let text = tokio::select! {
            _ = cancel.cancelled() => break,
            _ = &mut access_lost => {
                send_websocket_message_to_browser_with_timeout(
                    &mut sink,
                    stream_access_lost_close_frame(),
                )
                .await;
                break;
            }
            incoming = stream.next() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {
                    liveness.reset_silence_timer_after_client_frame();
                    continue;
                }
            },
            ping = liveness.wait_for_next_ping_or_silence_deadline() => match ping {
                Some(ping) => {
                    if !send_websocket_message_to_browser_with_timeout(&mut sink, ping).await {
                        break;
                    }
                    continue;
                }
                None => break,
            },
            event = rx.recv() => match event {
                Ok(env) => select(&env),
                Err(RecvError::Lagged(skipped)) => lagged(skipped),
                Err(RecvError::Closed) => break,
            },
        };
        if let Some(text) = text
            && !send_websocket_message_to_browser_with_timeout(
                &mut sink,
                Message::Text(text.into()),
            )
            .await
        {
            break;
        }
    }
}

pub const STREAM_ACCESS_ENDED_CLOSE_REASON: &str = "Access to this stream ended.";

pub fn stream_access_lost_close_frame() -> Message {
    Message::Close(Some(CloseFrame {
        code: close_code::POLICY,
        reason: STREAM_ACCESS_ENDED_CLOSE_REASON.into(),
    }))
}

pub fn skip_lag(_: u64) -> Option<String> {
    None
}

fn frame(env: &Envelope) -> Option<String> {
    Some(env.to_line())
}

#[utoipa::path(
    get,
    path = "/tasks/{project}/{task}/live",
    operation_id = "taskLive",
    tag = "tasks",
    summary = "Task live updates (WebSocket)",
    description = "Upgrades to a WebSocket emitting `evaluation_status_changed`,\n`build_status_changed`, `evaluation_progress` and `evaluation_activity`\nevents for this task, so the task page refetches on change instead of\npolling. `evaluation_progress` fires as builds and entry-points are\npersisted during the evaluation phase, before any build changes status,\nso the build/dependency totals grow live. `evaluation_activity` carries\nan `EvaluationProgress` snapshot at most once per second while the\nevaluation fetches or evaluates. Requires read access.",
    params(TaskPath),
    responses((status = SWITCHING_PROTOCOLS, description = "Switching Protocols - WebSocket upgrade successful"), WebError)
)]
pub async fn task_live_ws(
    TaskScope { project, task, .. }: TaskScope<Readable>,
    State(state): State<Arc<ServerState>>,
    credentials: WebSocketUpgradeCredentialRecheckRequestExtractor,
) -> WebResult<Response> {
    let task_id = task.id;
    let db = state.db();
    let (project_name, task_name) = (project.name, task.name.clone());
    let guard = credentials.watch_caller_access_with_recheck(Method::GET, move |user, api_key| {
        let (db, project_name, task_name) = (db.clone(), project_name.clone(), task_name.clone());
        async move {
            load_task(
                &db,
                Caller::from(&user),
                api_key.as_ref(),
                project_name,
                task_name,
                TaskAccess::Readable,
            )
            .await
            .map(drop)
        }
    });
    // The task's recent evaluations are seeded first. Build events must fire while the evaluation
    // itself is still in `Building`.
    let mut known: HashSet<EvaluationId> = gradient_db::evaluations::latest_of_task(
        &state.web_db,
        task.id,
        task.keep_evaluations.max(0) as u64,
    )
    .await
    .map(|rows| rows.into_iter().map(|e| e.id).collect())
    .unwrap_or_default();

    let rx = state.events.subscribe();
    let cancel = state.shutdown.token();
    let shutdown = state.shutdown.clone();
    Ok(guard.on_upgrade(move |socket, guard| async move {
        let _ = shutdown
            .spawn(live_stream(
                socket,
                rx,
                move |env| task_frame(env, task_id, &mut known),
                skip_lag,
                guard.wait_until_access_lost(),
                cancel,
            ))
            .await;
    }))
}

fn task_frame(
    env: &Envelope,
    task_id: TaskId,
    known: &mut HashSet<EvaluationId>,
) -> Option<String> {
    match &env.event {
        Event::EvaluationReported(evaluation::Reported {
            task: Some(t),
            evaluation_id,
            ..
        })
        | Event::EvaluationProgress(evaluation::Progress {
            task: Some(t),
            evaluation_id,
        })
        | Event::EvaluationActivity(evaluation::Activity {
            task: Some(t),
            evaluation_id,
            ..
        }) if *t == task_id => {
            known.insert(*evaluation_id);
            frame(env)
        }
        Event::BuildStatusChanged(b) if known.contains(&b.evaluation_id) => frame(env),
        _ => None,
    }
}

#[utoipa::path(
    get,
    path = "/evals/{evaluation}/live",
    operation_id = "evaluationLive",
    tag = "evals",
    summary = "Evaluation live updates (WebSocket)",
    description = "Upgrades to a WebSocket emitting `evaluation_status_changed`,\n`build_status_changed`, `evaluation_progress` and `evaluation_activity`\nevents for this evaluation. `evaluation_progress` fires as builds and\nentry-points are persisted during the evaluation phase so the build\nlist grows live. `evaluation_activity` carries an `EvaluationProgress`\nsnapshot at most once per second while the evaluation fetches or\nevaluates. Requires read access.",
    params(("evaluation" = EvaluationId, Path)),
    responses((status = SWITCHING_PROTOCOLS, description = "Switching Protocols - WebSocket upgrade successful"), WebError)
)]
pub async fn evaluation_live_ws(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
    credentials: WebSocketUpgradeCredentialRecheckRequestExtractor,
) -> WebResult<Response> {
    let ctx = EvalAccessContext::load(
        &state.web_db,
        evaluation_id,
        Caller::from(&maybe_user),
        api_key.as_ref(),
        ProjectItemAccessRequirement::PublicProjectOrCanViewProject,
    )
    .await?;
    let eval_id = ctx.evaluation.id;
    let db = state.web_db.clone();
    let guard = credentials.watch_caller_access_with_recheck(Method::GET, move |user, api_key| {
        let db = db.clone();
        async move {
            EvalAccessContext::load(
                &db,
                eval_id,
                Caller::from(&user),
                api_key.as_ref(),
                ProjectItemAccessRequirement::PublicProjectOrCanViewProject,
            )
            .await
            .map(drop)
        }
    });
    let rx = state.events.subscribe();
    let cancel = state.shutdown.token();
    let shutdown = state.shutdown.clone();
    Ok(guard.on_upgrade(move |socket, guard| async move {
        let _ = shutdown
            .spawn(live_stream(
                socket,
                rx,
                move |env| eval_frame(env, eval_id),
                skip_lag,
                guard.wait_until_access_lost(),
                cancel,
            ))
            .await;
    }))
}

#[utoipa::path(
    get,
    path = "/builds/{build}/live",
    operation_id = "buildLive",
    tag = "builds",
    summary = "Build live updates (WebSocket)",
    description = "Upgrades to a WebSocket emitting `build_status_changed` events\n(`build_id`, `derivation_build`, `evaluation_id`, `status`) for every\nbuild in this build's evaluation (covers its dependency graph), and\n`build_progress` events (`derivation_build`, `phase`, `bytes_done`,\n`bytes_total`, `paths_done`, `paths_total`) while this build prefetches\nits inputs, fetches a Substitute or Download, or uploads its outputs.\nRequires read access.",
    params(("build" = BuildJobId, Path)),
    responses((status = SWITCHING_PROTOCOLS, description = "Switching Protocols - WebSocket upgrade successful"), WebError)
)]
pub async fn build_live_ws(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
    credentials: WebSocketUpgradeCredentialRecheckRequestExtractor,
) -> WebResult<Response> {
    let ctx = BuildAccessContext::load(
        &state.web_db,
        build_id,
        Caller::from(&maybe_user),
        api_key.as_ref(),
        ProjectItemAccessRequirement::PublicProjectOrCanViewProject,
    )
    .await?;
    let db = state.web_db.clone();
    let guard = credentials.watch_caller_access_with_recheck(Method::GET, move |user, api_key| {
        let db = db.clone();
        async move {
            BuildAccessContext::load(
                &db,
                build_id,
                Caller::from(&user),
                api_key.as_ref(),
                ProjectItemAccessRequirement::PublicProjectOrCanViewProject,
            )
            .await
            .map(drop)
        }
    });
    let eval_id = ctx.build_job.evaluation;
    let shared_build = ctx.shared_build.id;
    let rx = state.events.subscribe();
    let cancel = state.shutdown.token();
    let shutdown = state.shutdown.clone();
    Ok(guard.on_upgrade(move |socket, guard| async move {
        let _ = shutdown
            .spawn(live_stream(
                socket,
                rx,
                move |env| build_frame(env, eval_id, shared_build),
                skip_lag,
                guard.wait_until_access_lost(),
                cancel,
            ))
            .await;
    }))
}

fn build_frame(
    env: &Envelope,
    eval_id: EvaluationId,
    shared_build: DerivationBuildId,
) -> Option<String> {
    match &env.event {
        Event::BuildProgress(p) if p.derivation_build == shared_build => frame(env),
        Event::EvaluationActivity(_) => None,
        _ => eval_frame(env, eval_id),
    }
}

fn eval_frame(env: &Envelope, eval_id: EvaluationId) -> Option<String> {
    let belongs = match &env.event {
        Event::EvaluationReported(e) => e.evaluation_id == eval_id,
        Event::EvaluationProgress(e) => e.evaluation_id == eval_id,
        Event::EvaluationActivity(e) => e.evaluation_id == eval_id,
        Event::BuildStatusChanged(b) => b.evaluation_id == eval_id,
        _ => false,
    };
    belongs.then(|| env.to_line())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::per_key_open_slot_limit::OpenSlotLimitPerKey;
    use futures::StreamExt;
    use futures::channel::mpsc::{self, SendError, UnboundedReceiver, UnboundedSender};
    use gradient_types::EvaluationProgress;
    use gradient_types::EventBus;
    use gradient_types::events::evaluation::{InputFetch, InputFetchState};
    use gradient_types::events::{build, cache};
    use gradient_util::shutdown::Shutdown;
    use std::convert::Infallible;
    use std::net::IpAddr;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::task::JoinHandle;
    use uuid::Uuid;

    fn env(event: impl Into<Event>) -> Envelope {
        Envelope::now(event.into())
    }
    fn eval_changed(eval: Uuid, task: Option<Uuid>) -> Envelope {
        env(evaluation::Reported {
            evaluation_id: EvaluationId::new(eval),
            task: task.map(TaskId::new),
            status: 3,
            ..Default::default()
        })
    }
    fn build_changed(eval: Uuid) -> Envelope {
        env(build::StatusChanged {
            build_id: BuildJobId::new(Uuid::from_u128(9)),
            derivation_build: DerivationBuildId::new(Uuid::from_u128(10)),
            evaluation_id: EvaluationId::new(eval),
            status: 2,
        })
    }
    fn progress(eval: Uuid, task: Option<Uuid>) -> Envelope {
        env(evaluation::Progress {
            evaluation_id: EvaluationId::new(eval),
            task: task.map(TaskId::new),
        })
    }
    fn eid(n: u128) -> EvaluationId {
        EvaluationId::new(Uuid::from_u128(n))
    }
    fn tid(n: u128) -> TaskId {
        TaskId::new(Uuid::from_u128(n))
    }
    fn activity(evaluation_id: EvaluationId, task: Option<TaskId>) -> Envelope {
        env(Event::EvaluationActivity(evaluation::Activity {
            evaluation_id,
            task,
            progress: EvaluationProgress::Evaluating { thunks: 9 },
        }))
    }

    #[test]
    fn the_task_and_evaluation_channels_forward_activity_and_the_build_channel_does_not() {
        assert!(eval_frame(&activity(eid(1), Some(tid(1))), eid(1)).is_some());
        assert!(eval_frame(&activity(eid(2), Some(tid(1))), eid(1)).is_none());
        let mut known = HashSet::new();
        assert!(task_frame(&activity(eid(1), Some(tid(1))), tid(1), &mut known).is_some());
        assert!(task_frame(&activity(eid(1), Some(tid(2))), tid(1), &mut known).is_none());
        let shared_build = DerivationBuildId::new(Uuid::from_u128(2));
        assert!(build_frame(&activity(eid(1), Some(tid(1))), eid(1), shared_build).is_none());
    }

    #[test]
    fn activity_serializes_its_kind_and_rows() {
        let line = env(Event::EvaluationActivity(evaluation::Activity {
            evaluation_id: eid(1),
            task: None,
            progress: EvaluationProgress::Fetching {
                inputs: vec![InputFetch {
                    name: "nixpkgs".into(),
                    state: InputFetchState::Fetching,
                    downloaded_bytes: 5,
                    expected_bytes: 0,
                }],
            },
        }))
        .to_line();
        assert!(line.contains(r#""event":"evaluation.activity""#), "{line}");
        assert!(line.contains(r#""kind":"fetching""#), "{line}");
        assert!(line.contains(r#""state":"Fetching""#), "{line}");
    }

    #[test]
    fn eval_channel_matches_only_its_evaluation() {
        let me = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        assert!(eval_frame(&eval_changed(me, None), eid(1)).is_some());
        assert!(eval_frame(&build_changed(me), eid(1)).is_some());
        assert!(eval_frame(&progress(me, None), eid(1)).is_some());
        assert!(eval_frame(&progress(other, None), eid(1)).is_none());
        assert!(eval_frame(&build_changed(other), eid(1)).is_none());
        assert!(eval_frame(&env(cache::Changed {}), eid(1)).is_none());
    }

    #[test]
    fn task_channel_learns_eval_ids_then_forwards_their_builds() {
        let task = Uuid::from_u128(7);
        let eval = Uuid::from_u128(8);
        let mut known = HashSet::new();

        assert!(task_frame(&build_changed(eval), tid(7), &mut known).is_none());
        assert!(task_frame(&eval_changed(eval, Some(task)), tid(7), &mut known).is_some());
        assert!(task_frame(&build_changed(eval), tid(7), &mut known).is_some());
        let foreign = Uuid::from_u128(99);
        assert!(
            task_frame(
                &eval_changed(foreign, Some(Uuid::from_u128(5))),
                tid(7),
                &mut known
            )
            .is_none()
        );
    }

    #[test]
    fn task_channel_forwards_build_transitions_for_seeded_evals() {
        let mut known = HashSet::from([eid(8)]);
        assert!(task_frame(&build_changed(Uuid::from_u128(8)), tid(7), &mut known).is_some());
    }

    #[test]
    fn task_channel_forwards_progress_and_learns_its_eval() {
        let task = Uuid::from_u128(7);
        let eval = Uuid::from_u128(8);
        let mut known = HashSet::new();

        assert!(task_frame(&progress(eval, Some(task)), tid(7), &mut known).is_some());
        assert!(task_frame(&build_changed(eval), tid(7), &mut known).is_some());
        assert!(
            task_frame(
                &progress(eval, Some(Uuid::from_u128(5))),
                tid(7),
                &mut HashSet::new()
            )
            .is_none()
        );
    }

    #[test]
    fn build_channel_adds_only_its_own_build_progress() {
        let shared_build = DerivationBuildId::new(Uuid::from_u128(2));
        let download = |derivation_build| {
            env(build::Progress {
                derivation_build,
                progress: BuildProgress {
                    phase: BuildProgressPhase::Download,
                    bytes_done: 1,
                    bytes_total: Some(4),
                    paths_done: 0,
                    paths_total: Some(1),
                },
            })
        };

        assert!(build_frame(&download(shared_build), eid(1), shared_build).is_some());
        assert!(
            build_frame(
                &download(DerivationBuildId::new(Uuid::from_u128(3))),
                eid(1),
                shared_build
            )
            .is_none()
        );
        assert!(build_frame(&build_changed(Uuid::from_u128(1)), eid(1), shared_build).is_some());
        assert!(eval_frame(&download(shared_build), eid(1)).is_none());
    }

    #[test]
    fn build_progress_serializes_its_numbers_flat() {
        let line = frame(&env(build::Progress {
            derivation_build: DerivationBuildId::new(Uuid::nil()),
            progress: BuildProgress {
                phase: BuildProgressPhase::Prefetch,
                bytes_done: 1,
                bytes_total: None,
                paths_done: 2,
                paths_total: Some(3),
            },
        }))
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["event"], "build.progress");
        assert_eq!(
            v["content"],
            serde_json::json!({
                "derivation_build": "00000000-0000-0000-0000-000000000000",
                "phase": "prefetch",
                "bytes_done": 1,
                "bytes_total": null,
                "paths_done": 2,
                "paths_total": 3
            })
        );
    }

    struct InMemoryWebSocket {
        from_client: UnboundedReceiver<Result<Message, Infallible>>,
        to_client: UnboundedSender<Message>,
    }

    impl Stream for InMemoryWebSocket {
        type Item = Result<Message, Infallible>;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            self.get_mut().from_client.poll_next_unpin(cx)
        }
    }

    impl Sink<Message> for InMemoryWebSocket {
        type Error = SendError;

        fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), SendError>> {
            Pin::new(&mut self.get_mut().to_client).poll_ready(cx)
        }

        fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), SendError> {
            Pin::new(&mut self.get_mut().to_client).start_send(item)
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), SendError>> {
            Pin::new(&mut self.get_mut().to_client).poll_flush(cx)
        }

        fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), SendError>> {
            Pin::new(&mut self.get_mut().to_client).poll_close(cx)
        }
    }

    fn spawn_live_loop_with_fake_socket_and_held_access_lost<T: Send + 'static>(
        held: T,
    ) -> (
        JoinHandle<()>,
        UnboundedSender<Result<Message, Infallible>>,
        UnboundedReceiver<Message>,
    ) {
        let (client_tx, from_client) = mpsc::unbounded();
        let (to_client, client_rx) = mpsc::unbounded();
        let bus = EventBus::new(16);
        let rx = bus.subscribe();
        let stream = Shutdown::new().spawn(live_stream(
            InMemoryWebSocket {
                from_client,
                to_client,
            },
            rx,
            |_| None,
            skip_lag,
            async move {
                let _held = (held, bus);
                std::future::pending::<()>().await;
            },
            CancellationToken::new(),
        ));
        (stream, client_tx, client_rx)
    }

    #[tokio::test(start_paused = true)]
    async fn fake_pong_client_keeps_stream_open_past_ping_intervals() {
        let (stream, client_tx, mut client_rx) =
            spawn_live_loop_with_fake_socket_and_held_access_lost(());
        Shutdown::new().spawn(async move {
            while let Some(message) = client_rx.next().await {
                if let Message::Ping(payload) = message {
                    let _ = client_tx.unbounded_send(Ok(Message::Pong(payload)));
                }
            }
        });

        tokio::time::sleep(LIVE_STREAM_PING_INTERVAL * 5).await;

        assert!(
            !stream.is_finished(),
            "Clients that respond to every Ping keep the stream open."
        );
    }

    #[tokio::test(start_paused = true)]
    async fn silent_peer_disconnect_after_silence_deadline_frees_anonymous_websocket_address_slot()
    {
        let limit = OpenSlotLimitPerKey::<IpAddr>::new(1);
        let address: IpAddr = "192.0.2.1".parse().unwrap();
        let permit = limit.try_acquire_open_slot_for_key(address).unwrap();
        assert!(limit.try_acquire_open_slot_for_key(address).is_none());
        let (stream, _client_tx, _client_rx) =
            spawn_live_loop_with_fake_socket_and_held_access_lost(permit);

        tokio::time::sleep(LIVE_STREAM_CLIENT_RECEIVE_TIMEOUT - Duration::from_secs(1)).await;
        assert!(
            !stream.is_finished(),
            "The stream is open until the silence deadline."
        );

        tokio::time::timeout(Duration::from_secs(2), stream)
            .await
            .expect("Streams end after the silence deadline.")
            .unwrap();
        assert!(
            limit.try_acquire_open_slot_for_key(address).is_some(),
            "The slot for the closed stream is free again."
        );
    }

    struct WebSocketPeerThatStopsReadingAfterConnect {
        from_client: UnboundedReceiver<Result<Message, Infallible>>,
    }

    impl Stream for WebSocketPeerThatStopsReadingAfterConnect {
        type Item = Result<Message, Infallible>;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            self.get_mut().from_client.poll_next_unpin(cx)
        }
    }

    impl Sink<Message> for WebSocketPeerThatStopsReadingAfterConnect {
        type Error = SendError;

        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), SendError>> {
            Poll::Pending
        }

        fn start_send(self: Pin<&mut Self>, _: Message) -> Result<(), SendError> {
            Ok(())
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), SendError>> {
            Poll::Pending
        }

        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), SendError>> {
            Poll::Pending
        }
    }

    #[tokio::test(start_paused = true)]
    async fn live_stream_to_client_that_stopped_reading_ends_within_send_timeout_and_frees_address_slot()
     {
        let limit = OpenSlotLimitPerKey::<IpAddr>::new(1);
        let address: IpAddr = "192.0.2.1".parse().unwrap();
        let permit = limit.try_acquire_open_slot_for_key(address).unwrap();
        let (client_tx, from_client) = mpsc::unbounded();
        let bus = EventBus::new(16);
        let rx = bus.subscribe();
        let stream = Shutdown::new().spawn(live_stream(
            WebSocketPeerThatStopsReadingAfterConnect { from_client },
            rx,
            |_| None,
            skip_lag,
            async move {
                let _held = (permit, bus);
                std::future::pending::<()>().await;
            },
            CancellationToken::new(),
        ));

        tokio::time::sleep(LIVE_STREAM_PING_INTERVAL - Duration::from_secs(1)).await;
        client_tx
            .unbounded_send(Ok(Message::Pong(Bytes::new())))
            .unwrap();

        tokio::time::timeout(
            Duration::from_secs(1)
                + LIVE_STREAM_OR_JOB_TERMINAL_BROWSER_MESSAGE_SEND_TIMEOUT
                + Duration::from_secs(1),
            stream,
        )
        .await
        .expect("Streams to clients that stopped reading close at the send timeout.")
        .unwrap();
        assert!(
            limit.try_acquire_open_slot_for_key(address).is_some(),
            "The slot for the client address is free again after the send timeout."
        );
    }

    #[tokio::test(start_paused = true)]
    async fn ping_send_failure_ends_live_stream_immediately() {
        let (stream, _client_tx, client_rx) =
            spawn_live_loop_with_fake_socket_and_held_access_lost(());
        drop(client_rx);

        tokio::time::timeout(LIVE_STREAM_PING_INTERVAL + Duration::from_secs(1), stream)
            .await
            .expect("Failures to send a ping end the stream immediately.")
            .unwrap();
    }
}
