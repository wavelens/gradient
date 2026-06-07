/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Job Board read endpoints: live dispatched jobs, per-job scoring detail,
//! connected workers, and the most expensive builds. Out-of-scope orgs are
//! masked: their jobs collapse to an aggregate count and foreign workers lose
//! their identity and live metrics.

use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult, require_superuser};
use crate::helpers::ok_json;
use crate::metrics_scope::MetricsScope;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::{Extension, Json};
use gradient_core::types::ids::{AcknowledgedDerivationId, DispatchedJobId};
use gradient_core::types::*;
use scheduler::{BoardEvent, Scheduler};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, Statement,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize)]
pub struct DispatchedJobSummary {
    pub id: Uuid,
    pub kind: i16,
    pub organization: Uuid,
    pub worker_id: String,
    pub score: f64,
    pub dispatched_at: String,
    pub build_id: Option<Uuid>,
    pub evaluation_id: Uuid,
}

#[derive(Serialize)]
pub struct DispatchedJobsResponse {
    pub jobs: Vec<DispatchedJobSummary>,
    /// In-flight jobs owned by orgs the caller can't see, shown only as a count.
    pub other_running: u64,
}

pub async fn get_dispatched_jobs(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
) -> WebResult<Json<BaseResponse<DispatchedJobsResponse>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let open = entity::dispatched_job::Entity::find()
        .filter(entity::dispatched_job::Column::FinishedAt.is_null())
        .order_by_desc(entity::dispatched_job::Column::DispatchedAt)
        .limit(500)
        .all(&state.web_db)
        .await?;

    let mut jobs = Vec::new();
    let mut other_running = 0u64;
    for j in open {
        if scope.allows(&Uuid::from(j.organization)) {
            jobs.push(DispatchedJobSummary {
                id: j.id.into(),
                kind: j.kind,
                organization: j.organization.into(),
                worker_id: j.worker_id,
                score: j.score,
                dispatched_at: j.dispatched_at.and_utc().to_rfc3339(),
                build_id: j.build_id.map(Into::into),
                evaluation_id: j.evaluation_id.into(),
            });
        } else {
            other_running += 1;
        }
    }
    Ok(ok_json(DispatchedJobsResponse { jobs, other_running }))
}

#[derive(Serialize)]
pub struct DispatchedJobDetail {
    pub id: Uuid,
    pub kind: i16,
    pub organization: Uuid,
    pub worker_id: String,
    pub score: f64,
    pub queued_at: String,
    pub dispatched_at: String,
    pub finished_at: Option<String>,
    pub build_id: Option<Uuid>,
    pub evaluation_id: Uuid,
    pub score_breakdown: serde_json::Value,
    pub worker_context: serde_json::Value,
    pub job_context: serde_json::Value,
    /// Runner-up candidates, superuser-only.
    pub candidates: Option<serde_json::Value>,
}

pub async fn get_dispatched_job(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<DispatchedJobDetail>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let j = entity::dispatched_job::Entity::find_by_id(DispatchedJobId::from(id))
        .one(&state.web_db)
        .await?
        .ok_or_else(|| WebError::not_found("Job"))?;
    if !scope.allows(&Uuid::from(j.organization)) {
        return Err(WebError::not_found("Job"));
    }
    Ok(ok_json(DispatchedJobDetail {
        id: j.id.into(),
        kind: j.kind,
        organization: j.organization.into(),
        worker_id: j.worker_id,
        score: j.score,
        queued_at: j.queued_at.and_utc().to_rfc3339(),
        dispatched_at: j.dispatched_at.and_utc().to_rfc3339(),
        finished_at: j.finished_at.map(|t| t.and_utc().to_rfc3339()),
        build_id: j.build_id.map(Into::into),
        evaluation_id: j.evaluation_id.into(),
        score_breakdown: j.score_breakdown,
        worker_context: j.worker_context,
        job_context: j.job_context,
        candidates: if scope.is_all() { j.candidates } else { None },
    }))
}

#[derive(Serialize)]
pub struct BoardWorker {
    /// `None` when the worker belongs to an org the caller can't see.
    pub id: Option<String>,
    pub organization: Option<Uuid>,
    pub draining: bool,
    pub assigned_jobs: i64,
    pub max_concurrent_builds: i64,
    pub eval: bool,
    pub fetch: bool,
    pub build: bool,
    pub architectures: Vec<String>,
    pub cpu_usage_pct: Option<f32>,
    pub ram_free_mb: Option<i64>,
    pub ram_total_mb: Option<i64>,
}

pub async fn get_board_workers(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<Vec<BoardWorker>>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let out = scheduler
        .board_workers()
        .await
        .into_iter()
        .map(|w| {
            let accessible = w
                .organization
                .map(|o| scope.allows(&Uuid::from(o)))
                .unwrap_or_else(|| scope.is_all());
            BoardWorker {
                id: accessible.then(|| w.id.clone()),
                organization: accessible.then(|| w.organization.map(Into::into)).flatten(),
                draining: w.draining,
                assigned_jobs: w.assigned_job_count as i64,
                max_concurrent_builds: w.max_concurrent_builds as i64,
                eval: w.capabilities.eval,
                fetch: w.capabilities.fetch,
                build: w.capabilities.build,
                architectures: if accessible { w.architectures } else { vec![] },
                cpu_usage_pct: accessible.then_some(w.cpu_usage_pct),
                ram_free_mb: accessible.then_some(w.ram_free_mb as i64),
                ram_total_mb: accessible.then_some(w.ram_total_mb as i64),
            }
        })
        .collect();
    Ok(ok_json(out))
}

#[derive(Deserialize)]
pub struct ExpensiveParams {
    pub window_days: Option<i64>,
    pub exclude_acknowledged: Option<bool>,
}

#[derive(Serialize)]
pub struct ExpensiveBuild {
    pub build_id: Uuid,
    pub organization: Uuid,
    pub name: String,
    pub build_time_ms: i64,
    pub worker: Option<String>,
}

pub async fn get_expensive_jobs(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<ExpensiveParams>,
) -> WebResult<Json<BaseResponse<Vec<ExpensiveBuild>>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let mut clauses = vec![
        "b.build_time_ms IS NOT NULL".to_string(),
        "b.status = 3".to_string(),
    ];
    if let Some(list) = scope.org_in_list() {
        if list.is_empty() {
            return Ok(ok_json(vec![]));
        }
        clauses.push(format!("d.organization IN ({list})"));
    }
    let window = params.window_days.unwrap_or(30).max(1);
    clauses.push(format!(
        "b.created_at >= (now() AT TIME ZONE 'UTC') - interval '{window} days'"
    ));
    if params.exclude_acknowledged.unwrap_or(true) {
        clauses.push(
            "NOT EXISTS (SELECT 1 FROM acknowledged_derivation a \
             WHERE a.derivation = d.id OR (a.pname IS NOT NULL AND a.pname = d.pname))"
                .to_string(),
        );
    }
    let sql = format!(
        "SELECT b.id, d.organization, d.name, b.build_time_ms, b.worker \
         FROM build b JOIN derivation d ON d.id = b.derivation \
         WHERE {} ORDER BY b.build_time_ms DESC LIMIT 20",
        clauses.join(" AND ")
    );
    let rows = state
        .web_db
        .query_all(Statement::from_string(DatabaseBackend::Postgres, sql))
        .await?;
    let out = rows
        .into_iter()
        .map(|r| ExpensiveBuild {
            build_id: r.try_get("", "id").unwrap_or_default(),
            organization: r.try_get("", "organization").unwrap_or_default(),
            name: r.try_get("", "name").unwrap_or_default(),
            build_time_ms: r.try_get("", "build_time_ms").unwrap_or(0),
            worker: r.try_get("", "worker").ok(),
        })
        .collect();
    Ok(ok_json(out))
}

pub async fn board_live_ws(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    ws: WebSocketUpgrade,
) -> Response {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user)
        .await
        .unwrap_or(MetricsScope::Orgs(vec![]));
    let rx = scheduler.board_events.subscribe();
    ws.on_upgrade(move |socket| board_live_loop(socket, rx, scope))
}

async fn board_live_loop(
    mut socket: WebSocket,
    mut rx: tokio::sync::broadcast::Receiver<BoardEvent>,
    scope: MetricsScope,
) {
    loop {
        match rx.recv().await {
            Ok(ev) => {
                if let Some(text) = mask_event(&ev, &scope)
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

/// Forward only events the caller may see: queue depth to everyone, per-org
/// events to members of that org (or superusers), worker disconnects to
/// superusers. Out-of-scope detail is dropped (the REST view supplies the
/// "other running" aggregate count).
fn mask_event(ev: &BoardEvent, scope: &MetricsScope) -> Option<String> {
    let visible = match ev {
        BoardEvent::QueueDepth { .. } => true,
        BoardEvent::JobDispatched { organization, .. } => scope.allows(organization),
        BoardEvent::WorkerConnected { organization, .. } => scope.allows(organization),
        BoardEvent::WorkerDisconnected { .. } => scope.is_all(),
    };
    visible.then(|| serde_json::to_string(ev).ok()).flatten()
}

#[derive(Serialize)]
pub struct AcknowledgedDerivationDto {
    pub id: Uuid,
    pub derivation: Option<Uuid>,
    pub pname: Option<String>,
    pub note: String,
    pub created_at: String,
}

pub async fn list_acknowledged(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Vec<AcknowledgedDerivationDto>>>> {
    require_superuser(&user)?;
    let rows = entity::acknowledged_derivation::Entity::find()
        .order_by_desc(entity::acknowledged_derivation::Column::CreatedAt)
        .all(&state.web_db)
        .await?;
    let out = rows
        .into_iter()
        .map(|a| AcknowledgedDerivationDto {
            id: a.id.into(),
            derivation: a.derivation.map(Into::into),
            pname: a.pname,
            note: a.note,
            created_at: a.created_at.and_utc().to_rfc3339(),
        })
        .collect();
    Ok(ok_json(out))
}

#[derive(Deserialize)]
pub struct CreateAcknowledged {
    pub derivation: Option<Uuid>,
    pub pname: Option<String>,
    pub note: String,
}

pub async fn create_acknowledged(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<CreateAcknowledged>,
) -> WebResult<Json<BaseResponse<Uuid>>> {
    require_superuser(&user)?;
    let id = AcknowledgedDerivationId::now_v7();
    let am = entity::acknowledged_derivation::ActiveModel {
        id: Set(id),
        derivation: Set(body.derivation.map(Into::into)),
        pname: Set(body.pname),
        note: Set(body.note),
        created_by: Set(user.id),
        created_at: Set(gradient_core::types::now()),
    };
    entity::acknowledged_derivation::Entity::insert(am)
        .exec(&state.web_db)
        .await?;
    Ok(ok_json(id.into()))
}

pub async fn delete_acknowledged(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<bool>>> {
    require_superuser(&user)?;
    entity::acknowledged_derivation::Entity::delete_by_id(AcknowledgedDerivationId::from(id))
        .exec(&state.web_db)
        .await?;
    Ok(ok_json(true))
}
