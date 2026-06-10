/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-resource live-update WebSocket channels. Each connection authorizes the
//! resource once at upgrade, then forwards only the `BoardEvent`s belonging to
//! that resource so the Angular pages can refetch on change instead of polling.

use crate::access::{Caller, ProjectAccess, load_project};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::WebResult;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use axum::Extension;
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

use super::builds::BuildAccessContext;
use super::evals::EvalAccessContext;

/// Forward events selected by `select` to the socket until either side closes.
/// A lagged receiver skips missed frames (the client refetches on the next one).
async fn live_stream<F>(mut socket: WebSocket, mut rx: BoardEventRx, mut select: F)
where
    F: FnMut(&BoardEvent) -> Option<String> + Send + 'static,
{
    loop {
        match rx.recv().await {
            Ok(ev) => {
                if let Some(text) = select(&ev)
                    && socket.send(Message::Text(text.into())).await.is_err()
                {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

type BoardEventRx = tokio::sync::broadcast::Receiver<BoardEvent>;

fn frame(ev: &BoardEvent) -> Option<String> {
    serde_json::to_string(ev).ok()
}

/// `GET /projects/{organization}/{project}/live` — evaluation and entry-point
/// build status changes for one project.
pub async fn project_live_ws(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project)): Path<(String, String)>,
    ws: WebSocketUpgrade,
) -> WebResult<Response> {
    let (_org, project) = load_project(
        &state,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Readable,
    )
    .await?;

    let project_id = project.id.into_inner();
    // Seed with the project's recent evaluations so build events fire even while
    // the evaluation itself stays in `Building`. New evaluations announce
    // themselves via their own status change and are added on the fly.
    let mut known: HashSet<Uuid> = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(project.keep_evaluations.max(0) as u64)
        .all(&state.web_db)
        .await
        .map(|rows| rows.into_iter().map(|e| e.id.into_inner()).collect())
        .unwrap_or_default();

    let rx = state.board_events.subscribe();
    Ok(ws.on_upgrade(move |socket| {
        live_stream(socket, rx, move |ev| project_frame(ev, project_id, &mut known))
    }))
}

/// Forward a project's own evaluation status changes (learning their ids) and
/// any build status change belonging to an evaluation we've seen for it.
fn project_frame(ev: &BoardEvent, project_id: Uuid, known: &mut HashSet<Uuid>) -> Option<String> {
    match ev {
        BoardEvent::EvaluationStatusChanged {
            project: Some(p),
            evaluation_id,
            ..
        } if *p == project_id => {
            known.insert(*evaluation_id);
            frame(ev)
        }
        BoardEvent::BuildStatusChanged { evaluation_id, .. } if known.contains(evaluation_id) => {
            frame(ev)
        }
        _ => None,
    }
}

/// `GET /evals/{evaluation}/live` — status changes for one evaluation and its
/// builds.
pub async fn evaluation_live_ws(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
    ws: WebSocketUpgrade,
) -> WebResult<Response> {
    let ctx = EvalAccessContext::load(&state, evaluation_id, &maybe_user, api_key.as_ref()).await?;
    let eval_id = ctx.evaluation.id.into_inner();
    let rx = state.board_events.subscribe();
    Ok(ws.on_upgrade(move |socket| live_stream(socket, rx, move |ev| eval_frame(ev, eval_id))))
}

/// `GET /builds/{build}/live` — build status changes for the build's evaluation,
/// which covers every node in its dependency graph.
pub async fn build_live_ws(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildId>,
    ws: WebSocketUpgrade,
) -> WebResult<Response> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user, api_key.as_ref()).await?;
    let eval_id = ctx.build.evaluation.into_inner();
    let rx = state.board_events.subscribe();
    Ok(ws.on_upgrade(move |socket| live_stream(socket, rx, move |ev| eval_frame(ev, eval_id))))
}

fn eval_frame(ev: &BoardEvent, eval_id: Uuid) -> Option<String> {
    match ev {
        BoardEvent::EvaluationStatusChanged { evaluation_id, .. }
        | BoardEvent::BuildStatusChanged { evaluation_id, .. }
            if *evaluation_id == eval_id =>
        {
            frame(ev)
        }
        _ => None,
    }
}

/// `GET /board/cache/live` — content-free pings when cache contents or stats
/// change. Subscribers refetch their own scope-filtered cache view.
pub async fn cache_live_ws(State(state): State<Arc<ServerState>>, ws: WebSocketUpgrade) -> Response {
    let rx = state.board_events.subscribe();
    ws.on_upgrade(move |socket| {
        live_stream(socket, rx, |ev| match ev {
            BoardEvent::CacheChanged => frame(ev),
            _ => None,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval_changed(eval: Uuid, project: Option<Uuid>) -> BoardEvent {
        BoardEvent::EvaluationStatusChanged {
            project,
            evaluation_id: eval,
            status: 3,
        }
    }
    fn build_changed(eval: Uuid) -> BoardEvent {
        BoardEvent::BuildStatusChanged {
            evaluation_id: eval,
            build_id: Uuid::from_u128(9),
            status: 2,
        }
    }

    #[test]
    fn eval_channel_matches_only_its_evaluation() {
        let me = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        assert!(eval_frame(&eval_changed(me, None), me).is_some());
        assert!(eval_frame(&build_changed(me), me).is_some());
        assert!(eval_frame(&build_changed(other), me).is_none());
        assert!(eval_frame(&BoardEvent::CacheChanged, me).is_none());
    }

    #[test]
    fn project_channel_learns_eval_ids_then_forwards_their_builds() {
        let project = Uuid::from_u128(7);
        let eval = Uuid::from_u128(8);
        let mut known = HashSet::new();

        // A build for an unknown evaluation is ignored.
        assert!(project_frame(&build_changed(eval), project, &mut known).is_none());
        // The evaluation's own change is forwarded and remembered.
        assert!(project_frame(&eval_changed(eval, Some(project)), project, &mut known).is_some());
        // Now its builds are forwarded.
        assert!(project_frame(&build_changed(eval), project, &mut known).is_some());
        // Another project's evaluation is ignored.
        let foreign = Uuid::from_u128(99);
        assert!(project_frame(&eval_changed(foreign, Some(Uuid::from_u128(5))), project, &mut known).is_none());
    }

    #[test]
    fn cache_changed_serializes_as_a_tagged_ping() {
        assert_eq!(frame(&BoardEvent::CacheChanged).unwrap(), r#"{"type":"cache_changed"}"#);
    }
}
