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
use gradient_types::ids::{AcknowledgedDerivationId, DispatchedJobId};
use gradient_types::*;
use gradient_core::ServerState;
use gradient_scheduler::{BoardEvent, Scheduler};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, IntoActiveModel, QueryFilter,
    QueryOrder, QuerySelect, Statement,
};
use gradient_entity::build_attempt;
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
    pub pname: Option<String>,
}

#[derive(Serialize)]
pub struct DispatchedJobsResponse {
    pub jobs: Vec<DispatchedJobSummary>,
    /// In-flight jobs owned by orgs the caller can't see, shown only as a count.
    pub other_running: u64,
}

#[derive(Serialize)]
pub struct PendingJobSummary {
    pub kind: i16,
    pub organization: Uuid,
    pub evaluation_id: Uuid,
    pub build_id: Option<Uuid>,
    pub queued_at: String,
    pub dependency_count: u32,
    pub pname: Option<String>,
}

#[derive(Serialize)]
pub struct PendingJobsResponse {
    pub jobs: Vec<PendingJobSummary>,
    /// Pending jobs owned by orgs the caller can't see, shown only as a count.
    pub other_pending: u64,
}

pub async fn get_pending_jobs(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<PendingJobsResponse>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let snapshot = scheduler.pending_jobs_snapshot().await;

    let mut jobs = Vec::new();
    let mut other_pending = 0u64;
    for j in snapshot {
        if scope.allows(&Uuid::from(j.organization)) {
            jobs.push(PendingJobSummary {
                kind: j.kind,
                organization: j.organization.into(),
                evaluation_id: j.evaluation_id.into(),
                build_id: j.build_id.map(Into::into),
                queued_at: j.queued_at.and_utc().to_rfc3339(),
                dependency_count: j.dependency_count,
                pname: j.pname.clone(),
            });
        } else {
            other_pending += 1;
        }
    }

    Ok(ok_json(PendingJobsResponse { jobs, other_pending }))
}

pub async fn get_dispatched_jobs(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
) -> WebResult<Json<BaseResponse<DispatchedJobsResponse>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let open = gradient_entity::dispatched_job::Entity::find()
        .filter(gradient_entity::dispatched_job::Column::FinishedAt.is_null())
        .order_by_desc(gradient_entity::dispatched_job::Column::DispatchedAt)
        .limit(500)
        .all(&state.web_db)
        .await?;

    let mut jobs = Vec::new();
    let mut other_running = 0u64;
    for j in open {
        if scope.allows(&Uuid::from(j.organization)) {
            let attempt = build_attempt::Entity::find()
                .filter(build_attempt::Column::DispatchedJob.eq(j.id))
                .one(&state.web_db)
                .await
                .ok()
                .flatten();

            let build_id = attempt.as_ref().map(|a| a.build.into());

            let pname = match attempt.as_ref() {
                Some(a) => {
                    let b = gradient_entity::build::Entity::find_by_id(a.build)
                        .one(&state.web_db)
                        .await
                        .ok()
                        .flatten();
                    match b {
                        Some(b) => gradient_entity::derivation::Entity::find_by_id(b.derivation)
                            .one(&state.web_db)
                            .await
                            .ok()
                            .flatten()
                            .and_then(|d| d.pname),
                        None => None,
                    }
                }
                None => None,
            };

            jobs.push(DispatchedJobSummary {
                id: j.id.into(),
                kind: j.kind,
                organization: j.organization.into(),
                worker_id: j.worker_id,
                score: j.score,
                dispatched_at: j.dispatched_at.and_utc().to_rfc3339(),
                build_id,
                evaluation_id: j.evaluation_id.into(),
                pname,
            });
        } else {
            other_running += 1;
        }
    }

    Ok(ok_json(DispatchedJobsResponse { jobs, other_running }))
}

#[derive(Serialize)]
pub struct AttemptSummary {
    pub dispatched_job_id: Uuid,
    pub substitute: bool,
    pub outcome: i32,
    pub reason: Option<i32>,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct DispatchedJobDetail {
    pub id: Uuid,
    pub kind: i16,
    pub organization: Uuid,
    pub organization_name: String,
    pub worker_id: String,
    pub score: f64,
    pub queued_at: String,
    pub dispatched_at: String,
    pub finished_at: Option<String>,
    pub build_id: Option<Uuid>,
    pub evaluation_id: Uuid,
    pub pname: Option<String>,
    pub score_breakdown: serde_json::Value,
    pub worker_context: serde_json::Value,
    pub job_context: serde_json::Value,
    pub instance_context: serde_json::Value,
    pub candidates: Option<serde_json::Value>,
    pub previous_attempts: Vec<AttemptSummary>,
}

pub async fn get_dispatched_job(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<DispatchedJobDetail>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let j = gradient_entity::dispatched_job::Entity::find_by_id(DispatchedJobId::from(id))
        .one(&state.web_db)
        .await?
        .ok_or_else(|| WebError::not_found("Job"))?;

    if !scope.allows(&Uuid::from(j.organization)) {
        return Err(WebError::not_found("Job"));
    }

    let organization_name = gradient_entity::organization::Entity::find_by_id(j.organization)
        .one(&state.web_db)
        .await?
        .map(|o| o.name)
        .unwrap_or_default();

    let this_attempt = build_attempt::Entity::find()
        .filter(build_attempt::Column::DispatchedJob.eq(j.id))
        .one(&state.web_db)
        .await
        .ok()
        .flatten();

    let build_id: Option<Uuid> = this_attempt.as_ref().map(|a| a.build.into());

    let pname = match build_id {
        Some(bid) => {
            let b = gradient_entity::build::Entity::find_by_id(gradient_types::ids::BuildId::from(bid))
                .one(&state.web_db).await.ok().flatten();
            match b {
                Some(b) => gradient_entity::derivation::Entity::find_by_id(b.derivation)
                    .one(&state.web_db).await.ok().flatten().and_then(|d| d.pname),
                None => None,
            }
        }
        None => None,
    };

    let previous_attempts = match build_id {
        Some(bid) => build_attempt::Entity::find()
            .filter(build_attempt::Column::Build.eq(gradient_types::ids::BuildId::from(bid)))
            .order_by_asc(build_attempt::Column::CreatedAt)
            .all(&state.web_db)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|a| AttemptSummary {
                dispatched_job_id: a.dispatched_job.into(),
                substitute: a.substitute,
                outcome: i32::from(a.outcome),
                reason: a.reason.map(i32::from),
                created_at: a.created_at.and_utc().to_rfc3339(),
            })
            .collect(),
        None => Vec::new(),
    };

    Ok(ok_json(DispatchedJobDetail {
        id: j.id.into(),
        kind: j.kind,
        organization: j.organization.into(),
        organization_name,
        worker_id: j.worker_id,
        score: j.score,
        queued_at: j.queued_at.and_utc().to_rfc3339(),
        dispatched_at: j.dispatched_at.and_utc().to_rfc3339(),
        finished_at: j.finished_at.map(|t| t.and_utc().to_rfc3339()),
        build_id,
        evaluation_id: j.evaluation_id.into(),
        pname,
        score_breakdown: j.score_breakdown,
        worker_context: j.worker_context,
        job_context: j.job_context,
        instance_context: j.instance_context.unwrap_or(serde_json::Value::Null),
        candidates: if scope.is_all() { j.candidates } else { None },
        previous_attempts,
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
        "ba.build_started_at IS NOT NULL AND ba.build_finished_at IS NOT NULL".to_string(),
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
        "SELECT b.id, d.organization, d.name, \
         EXTRACT(EPOCH FROM (ba.build_finished_at - ba.build_started_at))::bigint * 1000 AS build_time_ms, \
         dj.worker_id AS worker \
         FROM build b \
         JOIN derivation d ON d.id = b.derivation \
         JOIN LATERAL ( \
           SELECT ba2.build_started_at, ba2.build_finished_at, ba2.dispatched_job \
           FROM build_attempt ba2 WHERE ba2.build = b.id \
           ORDER BY ba2.created_at DESC LIMIT 1 \
         ) ba ON true \
         JOIN dispatched_job dj ON dj.id = ba.dispatched_job \
         WHERE {} ORDER BY build_time_ms DESC LIMIT 20",
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

#[derive(Deserialize)]
pub struct ScoringParams {
    pub window_hours: Option<i64>,
    pub limit: Option<u64>,
}

#[derive(Serialize)]
pub struct ScoreBucket {
    pub lo: f64,
    pub hi: f64,
    pub count: i64,
}

#[derive(Serialize)]
pub struct RuleContribution {
    pub rule: String,
    pub avg: f64,
    pub min: f64,
    pub max: f64,
}

#[derive(Serialize, Default)]
pub struct ScoringSummary {
    pub sample_size: i64,
    pub score_min: f64,
    pub score_max: f64,
    pub score_avg: f64,
    pub histogram: Vec<ScoreBucket>,
    pub rules: Vec<RuleContribution>,
}

/// Aggregate scoring view over recently dispatched jobs: a score histogram plus
/// the mean per-rule contribution, so operators can see how the policy scored
/// real dispatches without opening every job. Scope-masked to the caller's orgs.
pub async fn get_scoring_summary(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<ScoringParams>,
) -> WebResult<Json<BaseResponse<ScoringSummary>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let window = params.window_hours.unwrap_or(24).max(1);
    let limit = params.limit.unwrap_or(2000).min(10_000);

    let mut clauses = vec![format!(
        "dispatched_at >= (now() AT TIME ZONE 'UTC') - interval '{window} hours'"
    )];

    if let Some(list) = scope.org_in_list() {
        if list.is_empty() {
            return Ok(ok_json(ScoringSummary::default()));
        }

        clauses.push(format!("organization IN ({list})"));
    }

    let sql = format!(
        "SELECT score, score_breakdown FROM dispatched_job WHERE {} \
         ORDER BY dispatched_at DESC LIMIT {limit}",
        clauses.join(" AND ")
    );

    let rows = state
        .web_db
        .query_all(Statement::from_string(DatabaseBackend::Postgres, sql))
        .await?;

    let mut scores: Vec<f64> = Vec::with_capacity(rows.len());
    let mut rule_acc: std::collections::BTreeMap<String, (f64, i64, f64, f64)> =
        std::collections::BTreeMap::new();

    for r in &rows {
        scores.push(r.try_get::<f64>("", "score").unwrap_or(0.0));
        if let Ok(bd) = r.try_get::<serde_json::Value>("", "score_breakdown")
            && let Some(obj) = bd.get("rules").and_then(|v| v.as_object())
        {
            for (rule, val) in obj {
                if let Some(v) = val.as_f64() {
                    let e = rule_acc.entry(rule.clone()).or_insert((0.0, 0, f64::MAX, f64::MIN));
                    e.0 += v;
                    e.1 += 1;
                    e.2 = e.2.min(v);
                    e.3 = e.3.max(v);
                }
            }
        }
    }

    if scores.is_empty() {
        return Ok(ok_json(ScoringSummary::default()));
    }

    let n = scores.len() as f64;
    let lo = scores.iter().cloned().fold(f64::MAX, f64::min);
    let hi = scores.iter().cloned().fold(f64::MIN, f64::max);
    let avg = scores.iter().sum::<f64>() / n;

    const BINS: usize = 12;
    let span = (hi - lo).max(f64::EPSILON);
    let mut counts = vec![0i64; BINS];
    for s in &scores {
        let idx = (((s - lo) / span) * BINS as f64).floor() as usize;
        counts[idx.min(BINS - 1)] += 1;
    }

    let histogram = counts
        .into_iter()
        .enumerate()
        .map(|(i, count)| ScoreBucket {
            lo: lo + span * (i as f64) / BINS as f64,
            hi: lo + span * (i as f64 + 1.0) / BINS as f64,
            count,
        })
        .collect();

    let mut rules: Vec<RuleContribution> = rule_acc
        .into_iter()
        .map(|(rule, (sum, count, min, max))| RuleContribution {
            rule,
            avg: if count > 0 { sum / count as f64 } else { 0.0 },
            min,
            max,
        })
        .collect();

    rules.sort_by(|a, b| b.avg.abs().partial_cmp(&a.avg.abs()).unwrap_or(std::cmp::Ordering::Equal));

    Ok(ok_json(ScoringSummary {
        sample_size: scores.len() as i64,
        score_min: lo,
        score_max: hi,
        score_avg: avg,
        histogram,
        rules,
    }))
}

#[derive(Serialize)]
pub struct TopOrgBuildTime {
    pub organization: Uuid,
    pub total_build_ms: i64,
    pub build_count: i64,
}

/// Top organizations by cumulative build time in a window (superuser-only),
/// for the Expensive Jobs page.
pub async fn get_top_orgs_by_buildtime(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Query(params): Query<ExpensiveParams>,
) -> WebResult<Json<BaseResponse<Vec<TopOrgBuildTime>>>> {
    require_superuser(&user)?;
    let window = params.window_days.unwrap_or(30).max(1);
    let sql = format!(
        "SELECT d.organization, \
         sum(EXTRACT(EPOCH FROM (ba.build_finished_at - ba.build_started_at))::bigint * 1000)::bigint AS total, \
         count(*)::bigint AS cnt \
         FROM build b \
         JOIN derivation d ON d.id = b.derivation \
         JOIN LATERAL ( \
           SELECT ba2.build_started_at, ba2.build_finished_at \
           FROM build_attempt ba2 WHERE ba2.build = b.id \
           ORDER BY ba2.created_at DESC LIMIT 1 \
         ) ba ON true \
         WHERE b.status = 3 \
           AND ba.build_started_at IS NOT NULL AND ba.build_finished_at IS NOT NULL \
           AND ba.build_finished_at >= (now() AT TIME ZONE 'UTC') - interval '{window} days' \
         GROUP BY d.organization ORDER BY total DESC LIMIT 15"
    );

    let rows = state
        .web_db
        .query_all(Statement::from_string(DatabaseBackend::Postgres, sql))
        .await?;

    let out = rows
        .into_iter()
        .map(|r| TopOrgBuildTime {
            organization: r.try_get("", "organization").unwrap_or_default(),
            total_build_ms: r.try_get("", "total").unwrap_or(0),
            build_count: r.try_get("", "cnt").unwrap_or(0),
        })
        .collect();

    Ok(ok_json(out))
}

#[derive(Deserialize)]
pub struct ResourceParams {
    pub metric: String,
    pub window_days: Option<i64>,
    pub exclude_acknowledged: Option<bool>,
}

#[derive(Serialize)]
pub struct ExpensiveResource {
    pub derivation: Uuid,
    pub organization: Uuid,
    pub name: String,
    pub value: f64,
    pub unit: &'static str,
    pub worker: String,
}

/// Top derivations by a captured per-build resource (peak RAM, CPU time, total
/// disk bytes, or host network peak), read from `derivation_metric`. Org-scoped.
pub async fn get_expensive_by_resource(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<ResourceParams>,
) -> WebResult<Json<BaseResponse<Vec<ExpensiveResource>>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let (value_expr, unit, not_null): (&str, &'static str, &str) = match params.metric.as_str() {
        "ram" => ("dm.peak_ram_mb::double precision", "MB", "dm.peak_ram_mb IS NOT NULL"),
        "cpu" => ("dm.cpu_time_ms::double precision", "ms", "dm.cpu_time_ms IS NOT NULL"),
        "disk" => (
            "(coalesce(dm.disk_read_bytes,0) + coalesce(dm.disk_write_bytes,0))::double precision",
            "bytes",
            "(dm.disk_read_bytes IS NOT NULL OR dm.disk_write_bytes IS NOT NULL)",
        ),
        "network" => ("dm.peak_network_mbps", "Mbps", "dm.peak_network_mbps IS NOT NULL"),
        _ => return Err(WebError::not_found("Metric")),
    };

    let mut clauses = vec![not_null.to_string()];
    if let Some(list) = scope.org_in_list() {
        if list.is_empty() {
            return Ok(ok_json(vec![]));
        }

        clauses.push(format!("d.organization IN ({list})"));
    }

    let window = params.window_days.unwrap_or(30).max(1);
    clauses.push(format!(
        "dm.created_at >= (now() AT TIME ZONE 'UTC') - interval '{window} days'"
    ));

    if params.exclude_acknowledged.unwrap_or(true) {
        clauses.push(
            "NOT EXISTS (SELECT 1 FROM acknowledged_derivation a \
             WHERE a.derivation = d.id OR (a.pname IS NOT NULL AND a.pname = d.pname))"
                .to_string(),
        );
    }

    let sql = format!(
        "SELECT dm.derivation, d.organization, d.name, {value_expr} AS value, dm.worker_id \
         FROM derivation_metric dm JOIN derivation d ON d.id = dm.derivation \
         WHERE {} ORDER BY value DESC LIMIT 20",
        clauses.join(" AND ")
    );

    let rows = state
        .web_db
        .query_all(Statement::from_string(DatabaseBackend::Postgres, sql))
        .await?;

    let out = rows
        .into_iter()
        .map(|r| ExpensiveResource {
            derivation: r.try_get("", "derivation").unwrap_or_default(),
            organization: r.try_get("", "organization").unwrap_or_default(),
            name: r.try_get("", "name").unwrap_or_default(),
            value: r.try_get("", "value").unwrap_or(0.0),
            unit,
            worker: r.try_get("", "worker_id").unwrap_or_default(),
        })
        .collect();

    Ok(ok_json(out))
}

pub async fn board_live_ws(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    ws: WebSocketUpgrade,
) -> Response {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user)
        .await
        .unwrap_or(MetricsScope::Orgs(vec![]));

    let rx = state.board_events.subscribe();
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
        // Resource-scoped events are served by the per-resource /live channels.
        BoardEvent::EvaluationStatusChanged { .. }
        | BoardEvent::BuildStatusChanged { .. }
        | BoardEvent::CacheChanged => false,
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
    let rows = gradient_entity::acknowledged_derivation::Entity::find()
        .order_by_desc(gradient_entity::acknowledged_derivation::Column::CreatedAt)
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
    let am = gradient_entity::acknowledged_derivation::Model {
        id,
        derivation: body.derivation.map(Into::into),
        pname: body.pname,
        note: body.note,
        created_by: user.id,
        created_at: gradient_types::now(),
    }
    .into_active_model();

    gradient_entity::acknowledged_derivation::Entity::insert(am)
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
    gradient_entity::acknowledged_derivation::Entity::delete_by_id(AcknowledgedDerivationId::from(id))
        .exec(&state.web_db)
        .await?;

    Ok(ok_json(true))
}
