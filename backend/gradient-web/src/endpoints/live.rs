/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-resource live-update WebSocket channels. Each connection authorizes the
//! resource once at upgrade, then forwards only the events belonging to
//! that resource so the Angular pages can refetch on change instead of polling.

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

/// Forward events selected by `select` to the socket until either side closes.
/// `lagged` answers a receiver that fell behind: live channels skip (the client
/// refetches on the next frame), the firehose reports the gap.
///
/// The inbound half is polled purely to notice the client leaving: a loop that
/// only writes learns of a closed tab at the next event that fails to send, so
/// a quiet channel would hold its task and socket open indefinitely.
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

/// `GET /tasks/{project}/{task}/live` - evaluation and entry-point
/// build status changes for one task.
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
    // Seed with the task's recent evaluations so build events fire even while
    // the evaluation itself stays in `Building`. New evaluations announce
    // themselves via their own status change and are added on the fly.
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

/// Forward a task's own evaluation reports and progress (learning their ids)
/// and any build status change belonging to an evaluation we've seen for it.
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
        }) if *t == task_id => {
            known.insert(*evaluation_id);
            frame(env)
        }
        Event::BuildStatusChanged(b) if known.contains(&b.evaluation_id) => frame(env),
        _ => None,
    }
}

/// `GET /evals/{evaluation}/live` - status changes for one evaluation and its
/// builds.
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

/// `GET /builds/{build}/live` - build status changes for the build's evaluation,
/// which covers every node in its dependency graph, and the build's own
/// download progress.
pub async fn build_live_ws(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
    ws: WebSocketUpgrade,
) -> WebResult<Response> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user, api_key.as_ref()).await?;
    let eval_id = ctx.build_job.evaluation;
    let anchor = ctx.anchor.id;
    let rx = state.events.subscribe();
    let cancel = state.shutdown.token();
    let shutdown = state.shutdown.clone();
    Ok(ws.on_upgrade(move |socket| async move {
        let _ = shutdown
            .spawn(live_stream(
                socket,
                rx,
                move |env| build_frame(env, eval_id, anchor),
                skip_lag,
                cancel,
            ))
            .await;
    }))
}

fn build_frame(env: &Envelope, eval_id: EvaluationId, anchor: DerivationBuildId) -> Option<String> {
    match &env.event {
        Event::BuildProgress(p) if p.derivation_build == anchor => frame(env),
        _ => eval_frame(env, eval_id),
    }
}

fn eval_frame(env: &Envelope, eval_id: EvaluationId) -> Option<String> {
    let belongs = match &env.event {
        Event::EvaluationReported(e) => e.evaluation_id == eval_id,
        Event::EvaluationProgress(e) => e.evaluation_id == eval_id,
        Event::BuildStatusChanged(b) => b.evaluation_id == eval_id,
        _ => false,
    };
    belongs.then(|| env.to_line())
}

/// `GET /board/cache/live` - content-free pings when cache contents or stats
/// change. Subscribers refetch their own scope-filtered cache view.
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
    fn build_channel_adds_only_its_own_download_progress() {
        let anchor = DerivationBuildId::new(Uuid::from_u128(2));
        let download = |derivation_build| {
            env(build::Progress {
                derivation_build,
                progress: DownloadProgress {
                    downloaded: 1,
                    total: Some(4),
                },
            })
        };

        assert!(build_frame(&download(anchor), eid(1), anchor).is_some());
        assert!(
            build_frame(
                &download(DerivationBuildId::new(Uuid::from_u128(3))),
                eid(1),
                anchor
            )
            .is_none()
        );
        assert!(build_frame(&build_changed(Uuid::from_u128(1)), eid(1), anchor).is_some());
        assert!(eval_frame(&download(anchor), eid(1)).is_none());
    }

    #[test]
    fn build_progress_serializes_its_numbers_flat() {
        let line = frame(&env(build::Progress {
            derivation_build: DerivationBuildId::new(Uuid::nil()),
            progress: DownloadProgress {
                downloaded: 1,
                total: None,
            },
        }))
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["event"], "build.progress");
        assert_eq!(
            v["content"],
            serde_json::json!({
                "derivation_build": "00000000-0000-0000-0000-000000000000",
                "downloaded": 1,
                "total": null
            })
        );
    }
}
