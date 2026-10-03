/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, TaskAccess, load_task};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::WebResult;
use axum::Extension;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use gradient_core::ServerState;
use gradient_types::events::{Envelope, Event, EventRx, evaluation};
use gradient_types::*;
use gradient_util::shutdown::CancellationToken;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use std::collections::HashSet;
use std::sync::Arc;

use super::builds::BuildAccessContext;
use super::evals::EvalAccessContext;

/// The inbound half is polled only to notice the client leaving. A write-only loop would hold a
/// quiet channel's task and socket open indefinitely.
pub async fn live_stream<F, L>(
    socket: WebSocket,
    mut rx: EventRx,
    mut select: F,
    lagged: L,
    cancel: CancellationToken,
) where
    F: FnMut(&Envelope) -> Option<String> + Send + 'static,
    L: Fn(u64) -> Option<String> + Send + 'static,
{
    use futures::{SinkExt, StreamExt};
    use tokio::sync::broadcast::error::RecvError;

    let (mut sink, mut stream) = socket.split();
    loop {
        let text = tokio::select! {
            _ = cancel.cancelled() => break,
            incoming = stream.next() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => continue,
            },
            event = rx.recv() => match event {
                Ok(env) => select(&env),
                Err(RecvError::Lagged(skipped)) => lagged(skipped),
                Err(RecvError::Closed) => break,
            },
        };
        if let Some(text) = text
            && sink.send(Message::Text(text.into())).await.is_err()
        {
            break;
        }
    }
}

pub fn skip_lag(_: u64) -> Option<String> {
    None
}

fn frame(env: &Envelope) -> Option<String> {
    Some(env.to_line())
}

pub async fn task_live_ws(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((project, task)): Path<(String, String)>,
    ws: WebSocketUpgrade,
) -> WebResult<Response> {
    let (_project, task) = load_task(
        &state,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        project,
        task,
        TaskAccess::Readable,
    )
    .await?;

    let task_id = task.id;
    // The task's recent evaluations are seeded first. Build events must fire while the evaluation
    // itself is still in `Building`.
    let mut known: HashSet<EvaluationId> = EEvaluation::find()
        .filter(CEvaluation::Task.eq(task.id))
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(task.keep_evaluations.max(0) as u64)
        .all(&state.web_db)
        .await
        .map(|rows| rows.into_iter().map(|e| e.id).collect())
        .unwrap_or_default();

    let rx = state.events.subscribe();
    let cancel = state.shutdown.token();
    let shutdown = state.shutdown.clone();
    Ok(ws.on_upgrade(move |socket| async move {
        let _ = shutdown
            .spawn(live_stream(
                socket,
                rx,
                move |env| task_frame(env, task_id, &mut known),
                skip_lag,
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

pub async fn evaluation_live_ws(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
    ws: WebSocketUpgrade,
) -> WebResult<Response> {
    let ctx = EvalAccessContext::load(&state, evaluation_id, &maybe_user, api_key.as_ref()).await?;
    let eval_id = ctx.evaluation.id;
    let rx = state.events.subscribe();
    let cancel = state.shutdown.token();
    let shutdown = state.shutdown.clone();
    Ok(ws.on_upgrade(move |socket| async move {
        let _ = shutdown
            .spawn(live_stream(
                socket,
                rx,
                move |env| eval_frame(env, eval_id),
                skip_lag,
                cancel,
            ))
            .await;
    }))
}

pub async fn build_live_ws(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
    ws: WebSocketUpgrade,
) -> WebResult<Response> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user, api_key.as_ref()).await?;
    let eval_id = ctx.build_job.evaluation;
    let shared_build = ctx.shared_build.id;
    let rx = state.events.subscribe();
    let cancel = state.shutdown.token();
    let shutdown = state.shutdown.clone();
    Ok(ws.on_upgrade(move |socket| async move {
        let _ = shutdown
            .spawn(live_stream(
                socket,
                rx,
                move |env| build_frame(env, eval_id, shared_build),
                skip_lag,
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

pub async fn cache_live_ws(
    State(state): State<Arc<ServerState>>,
    ws: WebSocketUpgrade,
) -> Response {
    let rx = state.events.subscribe();
    let cancel = state.shutdown.token();
    let shutdown = state.shutdown.clone();
    ws.on_upgrade(move |socket| async move {
        let _ = shutdown
            .spawn(live_stream(
                socket,
                rx,
                |env| match env.event {
                    Event::CacheChanged(_) => frame(env),
                    _ => None,
                },
                skip_lag,
                cancel,
            ))
            .await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::EvaluationProgress;
    use gradient_types::events::evaluation::{InputFetch, InputFetchState};
    use gradient_types::events::{build, cache};
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
}
