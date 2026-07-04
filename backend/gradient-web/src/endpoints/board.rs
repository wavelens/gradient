/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Job Board read endpoints: live dispatched jobs, per-job scoring detail,
//! connected workers, and the most expensive builds. Out-of-scope orgs are
//! masked: their jobs collapse to an aggregate count and foreign workers lose
//! their identity and live metrics.

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::endpoints::evals::EvalAccessContext;
use crate::error::{WebError, WebResult, require_superuser};
use crate::helpers::ok_json;
use crate::metrics_scope::MetricsScope;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::{Extension, Json};
use gradient_types::ids::DispatchedJobId;
use gradient_types::*;
use gradient_core::ServerState;
use gradient_scheduler::{BoardEvent, Scheduler};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Statement,
};
use gradient_entity::{build_attempt, flake_output_node};
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
                kind: i16::from(j.kind),
                organization: j.organization.into(),
                evaluation_id: j.evaluation_id.into(),
                build_id: j.derivation_build.map(Into::into),
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

            let build_id = attempt.as_ref().map(|a| a.derivation_build.into());

            let pname = match attempt.as_ref() {
                Some(a) => {
                    let anchor = EDerivationBuild::find_by_id(a.derivation_build)
                        .one(&state.web_db)
                        .await
                        .ok()
                        .flatten();
                    match anchor {
                        Some(anchor) => gradient_entity::derivation::Entity::find_by_id(anchor.derivation)
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
                kind: i16::from(j.kind),
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
pub struct DecisionCandidateView {
    /// Ephemeral id navigable via `GET /board/jobs/{id}` to this candidate's
    /// score-breakdown detail, served from the in-memory decision ring.
    pub id: Uuid,
    pub job_id: String,
    pub kind: i16,
    pub organization: Uuid,
    pub build_id: Option<Uuid>,
    pub evaluation_id: Uuid,
    pub pname: Option<String>,
    pub score: f64,
    pub won: bool,
}

#[derive(Serialize)]
pub struct DispatchDecisionView {
    pub at: String,
    pub worker_id: String,
    pub kind: i16,
    pub winner: Option<String>,
    pub candidates: Vec<DecisionCandidateView>,
}

/// Recent dispatch decisions with every scored candidate, including rejected and
/// negative ones the dispatcher passed over. Superuser-only: candidates span all
/// orgs, and the view exists to tune cross-org scoring rules (#419).
pub async fn get_dispatch_decisions(
    Extension(user): Extension<MUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<Vec<DispatchDecisionView>>>> {
    require_superuser(&user)?;

    let views = scheduler
        .recent_decisions()
        .await
        .into_iter()
        .map(|d| DispatchDecisionView {
            at: d.at.and_utc().to_rfc3339(),
            worker_id: d.worker_id,
            kind: d.kind,
            winner: d.winner,
            candidates: d
                .candidates
                .into_iter()
                .map(|c| DecisionCandidateView {
                    id: c.id.into(),
                    job_id: c.job_id,
                    kind: c.kind,
                    organization: c.organization.into(),
                    build_id: c.derivation_build.map(Into::into),
                    evaluation_id: c.evaluation_id.into(),
                    pname: c.pname,
                    score: c.score,
                    won: c.won,
                })
                .collect(),
        })
        .collect();

    Ok(ok_json(views))
}

#[derive(Serialize)]
pub struct AttemptSummary {
    pub dispatched_job_id: Uuid,
    pub substitute: bool,
    pub outcome: i32,
    pub reason: Option<i32>,
    pub failure_message: Option<String>,
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
    /// True when this detail is an in-memory candidate the dispatcher scored but
    /// passed over (never written to `dispatched_job`). The UI labels it as such.
    pub passed_over: bool,
}

pub async fn get_dispatched_job(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path(id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<DispatchedJobDetail>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;

    // In-memory candidates (rejected and winning alike) carry an ephemeral id;
    // look there first, then fall back to the persisted `dispatched_job` row.
    if let Some(c) = scheduler.candidate_detail(DispatchedJobId::from(id)).await {
        if !scope.allows(&Uuid::from(c.organization)) {
            return Err(WebError::not_found("Job"));
        }

        let organization_name = gradient_entity::organization::Entity::find_by_id(c.organization)
            .one(&state.web_db)
            .await?
            .map(|o| o.name)
            .unwrap_or_default();

        return Ok(ok_json(DispatchedJobDetail {
            id: c.id.into(),
            kind: c.kind,
            organization: c.organization.into(),
            organization_name,
            worker_id: c.worker_id,
            score: c.score,
            queued_at: c.queued_at.and_utc().to_rfc3339(),
            dispatched_at: c.scored_at.and_utc().to_rfc3339(),
            finished_at: None,
            build_id: c.derivation_build.map(Into::into),
            evaluation_id: c.evaluation_id.into(),
            pname: c.pname,
            score_breakdown: c.score_breakdown,
            worker_context: c.worker_context,
            job_context: c.job_context,
            instance_context: c.instance_context,
            candidates: None,
            previous_attempts: Vec::new(),
            passed_over: !c.won,
        }));
    }

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

    let anchor_id: Option<DerivationBuildId> = this_attempt.as_ref().map(|a| a.derivation_build);
    let build_id: Option<Uuid> = anchor_id.map(Into::into);

    let pname = match anchor_id {
        Some(aid) => {
            let anchor = EDerivationBuild::find_by_id(aid)
                .one(&state.web_db).await.ok().flatten();
            match anchor {
                Some(anchor) => gradient_entity::derivation::Entity::find_by_id(anchor.derivation)
                    .one(&state.web_db).await.ok().flatten().and_then(|d| d.pname),
                None => None,
            }
        }
        None => None,
    };

    let previous_attempts = match anchor_id {
        Some(aid) => build_attempt::Entity::find()
            .filter(build_attempt::Column::DerivationBuild.eq(aid))
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
                failure_message: a.failure_message,
                created_at: a.created_at.and_utc().to_rfc3339(),
            })
            .collect(),
        None => Vec::new(),
    };

    Ok(ok_json(DispatchedJobDetail {
        id: j.id.into(),
        kind: i16::from(j.kind),
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
        passed_over: false,
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
                cpu_usage_pct: accessible.then_some(w.cpu_usage_pct).flatten(),
                ram_free_mb: accessible.then_some(w.ram_free_mb.map(|v| v as i64)).flatten(),
                ram_total_mb: accessible.then_some(w.ram_total_mb as i64),
            }
        })
        .collect();

    Ok(ok_json(out))
}

/// One axis of a worker-load radar: `in_flight` jobs of this kind against the
/// `capacity` (summed `max_concurrent_builds`) of the `workers` that can serve
/// it. Busy % is `in_flight / capacity`, computed by the client.
#[derive(Serialize, Debug, PartialEq)]
pub struct LoadBucket {
    pub key: String,
    pub in_flight: u32,
    pub capacity: u32,
    pub workers: u32,
}

/// Real per-capability / per-architecture / per-feature fleet load (#417).
/// Each breakdown answers "of the capacity that can serve this, how much is
/// running it" - so an operator can tell whether they are eval-, build-, or
/// architecture-bound rather than seeing one blended busy %.
#[derive(Serialize, Debug, PartialEq)]
pub struct WorkerLoad {
    pub by_capability: Vec<LoadBucket>,
    pub by_architecture: Vec<LoadBucket>,
    pub by_feature: Vec<LoadBucket>,
}

type LoadAcc = std::collections::HashMap<String, (u32, u32, u32)>;

fn bump_capacity(acc: &mut LoadAcc, key: &str, slots: u32) {
    let e = acc.entry(key.to_owned()).or_default();
    e.1 += slots;
    e.2 += 1;
}

fn bump_in_flight(acc: &mut LoadAcc, key: &str) {
    acc.entry(key.to_owned()).or_default().0 += 1;
}

fn buckets_sorted(acc: LoadAcc) -> Vec<LoadBucket> {
    let mut out: Vec<LoadBucket> = acc
        .into_iter()
        .map(|(key, (in_flight, capacity, workers))| LoadBucket { key, in_flight, capacity, workers })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// Aggregate already scope-filtered workers and in-flight jobs into the three
/// load breakdowns. Pure so it can be unit-tested without a scheduler or DB.
fn aggregate_worker_load(
    workers: &[&gradient_scheduler::WorkerInfo],
    jobs: &[&gradient_scheduler::BoardActiveJob],
) -> WorkerLoad {
    use gradient_entity::dispatched_job::DispatchedJobKind;

    let mut cap: LoadAcc = LoadAcc::new();
    let mut arch: LoadAcc = LoadAcc::new();
    let mut feat: LoadAcc = LoadAcc::new();

    for w in workers {
        let slots = w.max_concurrent_builds;
        if w.capabilities.eval {
            bump_capacity(&mut cap, "eval", slots);
        }
        if w.capabilities.fetch {
            bump_capacity(&mut cap, "fetch", slots);
        }
        if w.capabilities.build {
            bump_capacity(&mut cap, "build", slots);
        }
        for a in &w.architectures {
            bump_capacity(&mut arch, a, slots);
        }
        for f in &w.system_features {
            bump_capacity(&mut feat, f, slots);
        }
    }

    for j in jobs {
        match j.kind {
            DispatchedJobKind::Build => bump_in_flight(&mut cap, "build"),
            DispatchedJobKind::Eval => {
                if j.eval_task {
                    bump_in_flight(&mut cap, "eval");
                }
                if j.fetch_task {
                    bump_in_flight(&mut cap, "fetch");
                }
            }
        }
        if let Some(a) = &j.architecture
            && a != BUILTIN_ARCH
        {
            bump_in_flight(&mut arch, a);
        }
        for f in &j.required_features {
            bump_in_flight(&mut feat, f);
        }
    }

    let by_capability = ["eval", "fetch", "build"]
        .into_iter()
        .map(|k| {
            let (in_flight, capacity, workers) = cap.get(k).copied().unwrap_or_default();
            LoadBucket { key: k.to_owned(), in_flight, capacity, workers }
        })
        .collect();

    WorkerLoad {
        by_capability,
        by_architecture: buckets_sorted(arch),
        by_feature: buckets_sorted(feat),
    }
}

pub async fn get_board_worker_load(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<WorkerLoad>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let workers = scheduler.board_workers().await;
    let jobs = scheduler.board_active_jobs().await;

    let visible_workers: Vec<&gradient_scheduler::WorkerInfo> = workers
        .iter()
        .filter(|w| {
            w.organization
                .map(|o| scope.allows(&Uuid::from(o)))
                .unwrap_or_else(|| scope.is_all())
        })
        .collect();
    let visible_jobs: Vec<&gradient_scheduler::BoardActiveJob> = jobs
        .iter()
        .filter(|j| scope.allows(&Uuid::from(j.organization)))
        .collect();

    Ok(ok_json(aggregate_worker_load(&visible_workers, &visible_jobs)))
}

#[derive(Deserialize)]
pub struct ExpensiveParams {
    pub window_days: Option<i64>,
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
        format!(
            "b.status = {}",
            gradient_db::status_sql::build(gradient_entity::build::BuildStatus::Completed)
        ),
    ];

    if let Some(list) = scope.org_in_list() {
        if list.is_empty() {
            return Ok(ok_json(vec![]));
        }

        clauses.push(format!("pr.organization IN ({list})"));
    }

    let window = params.window_days.unwrap_or(30).max(1);
    clauses.push(format!(
        "b.created_at >= (now() AT TIME ZONE 'UTC') - interval '{window} days'"
    ));

    let sql = format!(
        "SELECT bj.id, pr.organization, d.name, \
         EXTRACT(EPOCH FROM (ba.build_finished_at - ba.build_started_at))::bigint * 1000 AS build_time_ms, \
         dj.worker_id AS worker \
         FROM build_job bj \
         JOIN derivation_build b ON b.id = bj.derivation_build \
         JOIN derivation d ON d.id = bj.derivation \
         JOIN evaluation ev ON ev.id = bj.evaluation \
         JOIN project pr ON pr.id = ev.project \
         JOIN LATERAL ( \
           SELECT ba2.build_started_at, ba2.build_finished_at, ba2.dispatched_job \
           FROM build_attempt ba2 WHERE ba2.derivation_build = b.id \
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

#[derive(Serialize)]
pub struct RuleDescription {
    pub rule: String,
    pub description: String,
}

/// Static catalog of every scoring rule and what it rewards or penalizes, so the
/// board UI can explain rule names in a help popup without duplicating the text.
pub async fn get_scoring_rules() -> WebResult<Json<BaseResponse<Vec<RuleDescription>>>> {
    let rules = gradient_score::rule_catalog()
        .into_iter()
        .map(|(rule, description)| RuleDescription {
            rule: rule.to_string(),
            description: description.to_string(),
        })
        .collect();

    Ok(ok_json(rules))
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
        "SELECT pr.organization, \
         sum(EXTRACT(EPOCH FROM (ba.build_finished_at - ba.build_started_at))::bigint * 1000)::bigint AS total, \
         count(*)::bigint AS cnt \
         FROM build_job bj \
         JOIN derivation_build b ON b.id = bj.derivation_build \
         JOIN evaluation ev ON ev.id = bj.evaluation \
         JOIN project pr ON pr.id = ev.project \
         JOIN LATERAL ( \
           SELECT ba2.build_started_at, ba2.build_finished_at \
           FROM build_attempt ba2 WHERE ba2.derivation_build = b.id \
           ORDER BY ba2.created_at DESC LIMIT 1 \
         ) ba ON true \
         WHERE b.status = {completed} \
           AND ba.build_started_at IS NOT NULL AND ba.build_finished_at IS NOT NULL \
           AND ba.build_finished_at >= (now() AT TIME ZONE 'UTC') - interval '{window} days' \
         GROUP BY pr.organization ORDER BY total DESC LIMIT 15",
        completed = gradient_db::status_sql::build(gradient_entity::build::BuildStatus::Completed),
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
    let org_filter = match scope.org_in_list() {
        Some(list) if list.is_empty() => return Ok(ok_json(vec![])),
        Some(list) => format!(" AND pr.organization IN ({list})"),
        None => String::new(),
    };

    let window = params.window_days.unwrap_or(30).max(1);
    clauses.push(format!(
        "dm.created_at >= (now() AT TIME ZONE 'UTC') - interval '{window} days'"
    ));

    // Derivations are global, so attribute each metric row to one producing org
    // (an in-scope one when scoped) via the build -> evaluation -> project chain.
    let sql = format!(
        "SELECT dm.derivation, pro.organization, d.name, {value_expr} AS value, dm.worker_id \
         FROM derivation_metric dm \
         JOIN derivation d ON d.id = dm.derivation \
         JOIN LATERAL ( \
           SELECT pr.organization \
           FROM build_job bj \
           JOIN evaluation ev ON ev.id = bj.evaluation \
           JOIN project pr ON pr.id = ev.project \
           WHERE bj.derivation = dm.derivation{org_filter} \
           LIMIT 1 \
         ) pro ON true \
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
    Extension(scheduler): Extension<Arc<Scheduler>>,
    ws: WebSocketUpgrade,
) -> Response {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user)
        .await
        .unwrap_or(MetricsScope::Orgs(vec![]));

    // Subscribe before snapshotting so no event is missed between the two.
    let rx = state.board_events.subscribe();
    let (workers, pending, active) = scheduler.metrics_snapshot().await;
    let initial = BoardEvent::QueueDepth { workers, pending, active };
    ws.on_upgrade(move |socket| board_live_loop(socket, rx, scope, initial))
}

async fn board_live_loop(
    mut socket: WebSocket,
    mut rx: tokio::sync::broadcast::Receiver<BoardEvent>,
    scope: MetricsScope,
    initial: BoardEvent,
) {
    // Send a queue-depth snapshot immediately so freshly-opened boards show
    // live counts without waiting for the next periodic broadcast.
    if let Some(text) = mask_event(&initial, &scope)
        && socket.send(Message::Text(text.into())).await.is_err()
    {
        return;
    }

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
        | BoardEvent::EvaluationProgress { .. }
        | BoardEvent::CacheChanged => false,
    };

    visible.then(|| serde_json::to_string(ev).ok()).flatten()
}

#[derive(Deserialize)]
pub struct EvalResourceParams {
    pub metric: String,
    pub window_days: Option<i64>,
}

#[derive(Serialize)]
pub struct ExpensiveEval {
    pub evaluation: Uuid,
    pub organization: Uuid,
    pub name: String,
    pub value: f64,
    pub unit: &'static str,
    pub worker: String,
}

/// Maps a metric key to its SQL value expression + unit. Pure + tested so the
/// metric param can never inject SQL (closed allow-list).
fn eval_metric_expr(metric: &str) -> Option<(&'static str, &'static str)> {
    Some(match metric {
        "rss" => ("em.peak_rss_mb::double precision", "MB"),
        "heap" => ("em.peak_heap_mb::double precision", "MB"),
        "thunks" => ("em.total_thunks::double precision", "count"),
        "fncalls" => ("em.fn_calls::double precision", "count"),
        "alloc" => ("em.alloc_bytes::double precision", "bytes"),
        "time" => ("em.total_eval_ms::double precision", "ms"),
        _ => return None,
    })
}

/// Top evaluations by a captured per-eval resource (peak RSS/heap, thunks, fn
/// calls, allocated bytes, or total eval time) from `evaluation_metric`,
/// org-scoped through the evaluation's project.
pub async fn get_expensive_evals_by_resource(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Query(params): Query<EvalResourceParams>,
) -> WebResult<Json<BaseResponse<Vec<ExpensiveEval>>>> {
    let (value_expr, unit) =
        eval_metric_expr(&params.metric).ok_or_else(|| WebError::not_found("Metric"))?;
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;

    let mut clauses = vec![];
    if let Some(list) = scope.org_in_list() {
        if list.is_empty() {
            return Ok(ok_json(vec![]));
        }

        clauses.push(format!("p.organization IN ({list})"));
    }

    let window = params.window_days.unwrap_or(30).max(1);
    clauses.push(format!(
        "em.created_at >= (now() AT TIME ZONE 'UTC') - interval '{window} days'"
    ));

    let sql = format!(
        "SELECT em.evaluation, p.organization, ev.wildcard AS name, {value_expr} AS value, em.worker_id \
         FROM evaluation_metric em \
         JOIN evaluation ev ON ev.id = em.evaluation \
         JOIN project p ON p.id = ev.project \
         WHERE {} ORDER BY value DESC LIMIT 20",
        clauses.join(" AND ")
    );

    let rows = state
        .web_db
        .query_all(Statement::from_string(DatabaseBackend::Postgres, sql))
        .await?;

    let out = rows
        .into_iter()
        .map(|r| ExpensiveEval {
            evaluation: r.try_get("", "evaluation").unwrap_or_default(),
            organization: r.try_get("", "organization").unwrap_or_default(),
            name: r.try_get("", "name").unwrap_or_default(),
            value: r.try_get("", "value").unwrap_or(0.0),
            unit,
            worker: r.try_get("", "worker_id").unwrap_or_default(),
        })
        .collect();

    Ok(ok_json(out))
}

#[derive(Serialize)]
pub struct FlakeGraphNode {
    pub path: String,
    pub parent: Option<String>,
    pub name: String,
    pub kind: String,
    pub is_derivation: bool,
    pub drv_path: Option<String>,
}

fn to_graph_node(n: flake_output_node::Model) -> FlakeGraphNode {
    FlakeGraphNode {
        path: n.path,
        parent: n.parent,
        name: n.name,
        kind: n.kind,
        is_derivation: n.is_derivation,
        drv_path: n.drv_path.map(|p| {
            gradient_entity::StorePath::parse(&p)
                .map(|sp| sp.base())
                .unwrap_or(p)
        }),
    }
}

/// GET /evals/{evaluation}/flake-graph - the eval's walked flake output graph.
pub async fn get_eval_flake_graph(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<Vec<FlakeGraphNode>>>> {
    let _ctx = EvalAccessContext::load(&state, evaluation_id, &maybe_user, api_key.as_ref()).await?;
    let rows = flake_output_node::Entity::find()
        .filter(flake_output_node::Column::Evaluation.eq(evaluation_id))
        .all(&state.web_db)
        .await?;

    let out = rows.into_iter().map(to_graph_node).collect();
    Ok(ok_json(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::dispatched_job::DispatchedJobKind;
    use gradient_scheduler::{BoardActiveJob, WorkerInfo};
    use gradient_types::ids::OrganizationId;
    use gradient_types::proto::GradientCapabilities;

    fn worker(
        eval: bool,
        fetch: bool,
        build: bool,
        arch: &[&str],
        features: &[&str],
        slots: u32,
    ) -> WorkerInfo {
        WorkerInfo {
            id: "w".into(),
            capabilities: GradientCapabilities { eval, fetch, build, ..Default::default() },
            architectures: arch.iter().map(|s| s.to_string()).collect(),
            system_features: features.iter().map(|s| s.to_string()).collect(),
            max_concurrent_builds: slots,
            assigned_job_count: 0,
            draining: false,
            authorized_peers: None,
            organization: None,
            cpu_usage_pct: None,
            ram_free_mb: None,
            ram_total_mb: 0,
            disk_speed_mbps: None,
            network_speed_mbps: None,
        }
    }

    fn build_job(arch: &str, features: &[&str]) -> BoardActiveJob {
        BoardActiveJob {
            worker_id: "w".into(),
            organization: OrganizationId::now_v7(),
            kind: DispatchedJobKind::Build,
            architecture: Some(arch.into()),
            required_features: features.iter().map(|s| s.to_string()).collect(),
            fetch_task: false,
            eval_task: false,
        }
    }

    fn flake_job(eval_task: bool, fetch_task: bool) -> BoardActiveJob {
        BoardActiveJob {
            worker_id: "w".into(),
            organization: OrganizationId::now_v7(),
            kind: DispatchedJobKind::Eval,
            architecture: None,
            required_features: vec![],
            fetch_task,
            eval_task,
        }
    }

    fn bucket<'a>(load: &'a [LoadBucket], key: &str) -> &'a LoadBucket {
        load.iter().find(|b| b.key == key).expect("bucket present")
    }

    #[test]
    fn worker_load_diverges_per_capability_and_architecture() {
        // One all-round worker (8 slots) and one build-only worker (4 slots):
        // heavy on builds, light on eval/fetch - the build-bound case from #417.
        let all_round = worker(true, true, true, &["x86_64-linux"], &["kvm"], 8);
        let build_only = worker(false, false, true, &["aarch64-linux"], &[], 4);
        let workers: Vec<&WorkerInfo> = vec![&all_round, &build_only];

        let mut jobs = Vec::new();
        for i in 0..6 {
            jobs.push(build_job("x86_64-linux", if i < 3 { &["kvm"] } else { &[] }));
        }
        jobs.push(build_job("aarch64-linux", &[]));
        jobs.push(build_job("aarch64-linux", &[]));
        jobs.push(flake_job(true, false));
        jobs.push(flake_job(false, true));
        let job_refs: Vec<&BoardActiveJob> = jobs.iter().collect();

        let load = aggregate_worker_load(&workers, &job_refs);

        let cap = &load.by_capability;
        assert_eq!(cap.iter().map(|b| b.key.as_str()).collect::<Vec<_>>(), ["eval", "fetch", "build"]);
        assert_eq!(*bucket(cap, "eval"), LoadBucket { key: "eval".into(), in_flight: 1, capacity: 8, workers: 1 });
        assert_eq!(*bucket(cap, "fetch"), LoadBucket { key: "fetch".into(), in_flight: 1, capacity: 8, workers: 1 });
        assert_eq!(*bucket(cap, "build"), LoadBucket { key: "build".into(), in_flight: 8, capacity: 12, workers: 2 });

        let arch = &load.by_architecture;
        assert_eq!(*bucket(arch, "x86_64-linux"), LoadBucket { key: "x86_64-linux".into(), in_flight: 6, capacity: 8, workers: 1 });
        assert_eq!(*bucket(arch, "aarch64-linux"), LoadBucket { key: "aarch64-linux".into(), in_flight: 2, capacity: 4, workers: 1 });

        let feat = &load.by_feature;
        assert_eq!(*bucket(feat, "kvm"), LoadBucket { key: "kvm".into(), in_flight: 3, capacity: 8, workers: 1 });
    }

    #[test]
    fn worker_load_ignores_builtin_arch_and_shows_empty_capacity() {
        let w = worker(true, false, true, &["x86_64-linux"], &[], 2);
        let workers: Vec<&WorkerInfo> = vec![&w];
        // A builtin build must not create a phantom architecture bucket.
        let jobs = [build_job(BUILTIN_ARCH, &[])];
        let job_refs: Vec<&BoardActiveJob> = jobs.iter().collect();

        let load = aggregate_worker_load(&workers, &job_refs);
        assert!(load.by_architecture.iter().all(|b| b.key != BUILTIN_ARCH));
        // fetch axis stays present with zero capacity so the radar keeps 3 axes.
        assert_eq!(*bucket(&load.by_capability, "fetch"), LoadBucket { key: "fetch".into(), in_flight: 0, capacity: 0, workers: 0 });
    }

    #[test]
    fn eval_metric_expr_known_keys_have_units() {
        assert_eq!(eval_metric_expr("rss").unwrap().1, "MB");
        assert_eq!(eval_metric_expr("heap").unwrap().1, "MB");
        assert_eq!(eval_metric_expr("thunks").unwrap().1, "count");
        assert_eq!(eval_metric_expr("fncalls").unwrap().1, "count");
        assert_eq!(eval_metric_expr("alloc").unwrap().1, "bytes");
        assert_eq!(eval_metric_expr("time").unwrap().1, "ms");
    }

    #[test]
    fn eval_metric_expr_unknown_is_none() {
        assert!(eval_metric_expr("bogus").is_none());
        assert!(eval_metric_expr("").is_none());
    }

    #[test]
    fn flake_output_node_maps_to_graph_node() {
        let model = flake_output_node::Model {
            id: FlakeOutputNodeId::now_v7(),
            evaluation: EvaluationId::now_v7(),
            path: "packages.x86_64-linux.hello".into(),
            parent: Some("packages.x86_64-linux".into()),
            name: "hello".into(),
            kind: "derivation".into(),
            is_derivation: true,
            drv_path: Some("/nix/store/abc-hello.drv".into()),
        };
        let node = to_graph_node(model);
        assert_eq!(node.path, "packages.x86_64-linux.hello");
        assert_eq!(node.parent.as_deref(), Some("packages.x86_64-linux"));
        assert_eq!(node.name, "hello");
        assert_eq!(node.kind, "derivation");
        assert!(node.is_derivation);
        assert_eq!(node.drv_path.as_deref(), Some("abc-hello.drv"));
    }
}
