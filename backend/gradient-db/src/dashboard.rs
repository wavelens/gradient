/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::{NaiveDate, NaiveDateTime};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, QueryResult, Value};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StarKind {
    Project,
    Task,
    Cache,
}

#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct StarredTask {
    pub project: String,
    pub task: String,
}

#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct StarredNames {
    pub projects: Vec<String>,
    pub tasks: Vec<StarredTask>,
    pub caches: Vec<String>,
}

crate::sql! {
    STAR_PROJECT = "INSERT INTO user_project_star (\"user\", project) VALUES ($1, $2) \
        ON CONFLICT DO NOTHING",
        params = [UserId, ProjectId],
        tier = Hot;

    STAR_TASK = "INSERT INTO user_task_star (\"user\", task) VALUES ($1, $2) \
        ON CONFLICT DO NOTHING",
        params = [UserId, TaskId],
        tier = Hot;

    STAR_CACHE = "INSERT INTO user_cache_star (\"user\", cache) VALUES ($1, $2) \
        ON CONFLICT DO NOTHING",
        params = [UserId, CacheId],
        tier = Hot;

    UNSTAR_PROJECT = "DELETE FROM user_project_star WHERE \"user\" = $1 AND project = $2",
        params = [UserId, ProjectId],
        tier = Hot;

    UNSTAR_TASK = "DELETE FROM user_task_star WHERE \"user\" = $1 AND task = $2",
        params = [UserId, TaskId],
        tier = Hot;

    UNSTAR_CACHE = "DELETE FROM user_cache_star WHERE \"user\" = $1 AND cache = $2",
        params = [UserId, CacheId],
        tier = Hot;

    STARRED_PROJECTS = "SELECT p.name FROM user_project_star s JOIN project p ON p.id = s.project \
        WHERE s.\"user\" = $1 AND ($2 OR p.public OR EXISTS (SELECT 1 FROM project_user pu \
        WHERE pu.project = p.id AND pu.\"user\" = $1)) ORDER BY p.name",
        params = [UserId, Bool(false)],
        tier = Hot;

    STARRED_TASKS = "SELECT p.name AS project, t.name FROM user_task_star s \
        JOIN task t ON t.id = s.task JOIN project p ON p.id = t.project \
        WHERE s.\"user\" = $1 AND ($2 OR p.public OR EXISTS (SELECT 1 FROM project_user pu \
        WHERE pu.project = p.id AND pu.\"user\" = $1)) ORDER BY p.name, t.name",
        params = [UserId, Bool(false)],
        tier = Hot;

    STARRED_CACHES = "SELECT c.name FROM user_cache_star s JOIN cache c ON c.id = s.cache \
        WHERE s.\"user\" = $1 AND ($2 OR c.public OR c.created_by = $1 \
        OR EXISTS (SELECT 1 FROM cache_user cu WHERE cu.cache = c.id AND cu.\"user\" = $1) \
        OR EXISTS (SELECT 1 FROM project_cache pc JOIN project_user pu ON pu.project = pc.project \
        WHERE pc.cache = c.id AND pu.\"user\" = $1)) ORDER BY c.name",
        params = [UserId, Bool(false)],
        tier = Hot;
}

fn pair(user: UserId, target: Uuid) -> [Value; 2] {
    [user.into_inner().into(), target.into()]
}

pub async fn star<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    kind: StarKind,
    target: Uuid,
) -> Result<(), DbErr> {
    let query = match kind {
        StarKind::Project => &STAR_PROJECT,
        StarKind::Task => &STAR_TASK,
        StarKind::Cache => &STAR_CACHE,
    };
    db.execute_raw(query.bind(pair(user, target))).await?;
    Ok(())
}

pub async fn unstar<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    kind: StarKind,
    target: Uuid,
) -> Result<(), DbErr> {
    let query = match kind {
        StarKind::Project => &UNSTAR_PROJECT,
        StarKind::Task => &UNSTAR_TASK,
        StarKind::Cache => &UNSTAR_CACHE,
    };
    db.execute_raw(query.bind(pair(user, target))).await?;
    Ok(())
}

fn names(rows: &[QueryResult]) -> Result<Vec<String>, DbErr> {
    rows.iter().map(|r| r.try_get("", "name")).collect()
}

fn starred_task(row: &QueryResult) -> Result<StarredTask, DbErr> {
    Ok(StarredTask {
        project: row.try_get("", "project")?,
        task: row.try_get("", "name")?,
    })
}

/// Only targets the user can still read: losing access hides a star, it does not delete it.
pub async fn starred_names<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    superuser: bool,
) -> Result<StarredNames, DbErr> {
    let bind = || -> [Value; 2] { [user.into_inner().into(), superuser.into()] };
    let projects = names(&db.query_all_raw(STARRED_PROJECTS.bind(bind())).await?)?;
    let tasks = db
        .query_all_raw(STARRED_TASKS.bind(bind()))
        .await?
        .iter()
        .map(starred_task)
        .collect::<Result<_, _>>()?;
    let caches = names(&db.query_all_raw(STARRED_CACHES.bind(bind())).await?)?;
    Ok(StarredNames {
        projects,
        tasks,
        caches,
    })
}

const NON_PR: &str = "tt.trigger_type IS DISTINCT FROM 2";

fn terminal() -> String {
    crate::status_sql::eval_in(&EvaluationStatus::TERMINAL)
}

fn task_facts_sql() -> String {
    format!(
        "WITH viewer_tasks AS ( \
            SELECT t.id FROM task t JOIN project_user pu ON pu.project = t.project WHERE pu.\"user\" = $1 \
            UNION SELECT s.task FROM user_task_star s JOIN task st ON st.id = s.task \
            JOIN project sproj ON sproj.id = st.project WHERE s.\"user\" = $1 AND sproj.public) \
        SELECT p.name AS project, t.name AS task, \
            EXISTS (SELECT 1 FROM user_task_star s WHERE s.\"user\" = $1 AND s.task = t.id) AS starred, \
            l.id AS latest_id, l.status AS latest_status, encode(c.hash, 'hex') AS latest_commit, \
            l.created_at AS latest_created_at, pv.id AS previous_id, \
            coalesce(a.recent_14d, 0) AS recent_14d, coalesce(a.last_28d, 0) AS last_28d, \
            coalesce(a.completed_30d, 0) AS completed_30d, coalesce(a.failed_30d, 0) AS failed_30d, \
            sp.speed_ms \
        FROM viewer_tasks v JOIN task t ON t.id = v.id JOIN project p ON p.id = t.project \
        LEFT JOIN LATERAL (SELECT e.id, e.status, e.commit, e.created_at FROM evaluation e \
            LEFT JOIN task_trigger tt ON tt.id = e.\"trigger\" WHERE e.task = t.id AND {NON_PR} \
            ORDER BY e.created_at DESC LIMIT 1) l ON true \
        LEFT JOIN commit c ON c.id = l.commit \
        LEFT JOIN LATERAL (SELECT e.id FROM evaluation e LEFT JOIN task_trigger tt ON tt.id = e.\"trigger\" \
            WHERE e.task = t.id AND {NON_PR} AND e.status IN ({term}) AND e.created_at < l.created_at \
            ORDER BY e.created_at DESC LIMIT 1) pv ON true \
        LEFT JOIN LATERAL (SELECT \
            count(*) FILTER (WHERE e.created_at > now() - interval '14 days') AS recent_14d, \
            count(*) FILTER (WHERE e.created_at > now() - interval '28 days') AS last_28d, \
            count(*) FILTER (WHERE e.status = {completed}) AS completed_30d, \
            count(*) FILTER (WHERE e.status = {failed}) AS failed_30d \
            FROM evaluation e LEFT JOIN task_trigger tt ON tt.id = e.\"trigger\" \
            WHERE e.task = t.id AND {NON_PR} AND e.created_at > now() - interval '30 days') a ON true \
        LEFT JOIN LATERAL (SELECT (avg(extract(epoch FROM (r.finished_at - r.created_at))) * 1000)::bigint AS speed_ms \
            FROM (SELECT e.created_at, e.finished_at FROM evaluation e LEFT JOIN task_trigger tt ON tt.id = e.\"trigger\" \
                WHERE e.task = t.id AND {NON_PR} AND e.status IN ({term}) AND e.finished_at IS NOT NULL \
                ORDER BY e.created_at DESC LIMIT 30) r) sp ON true",
        term = terminal(),
        completed = crate::status_sql::eval(EvaluationStatus::Completed),
        failed = crate::status_sql::eval(EvaluationStatus::Failed),
    )
}

fn outcomes_sql() -> String {
    use BuildStatus::*;
    format!(
        "SELECT ep.evaluation AS id, \
            count(*) FILTER (WHERE db.status IN ({ok}))::bigint AS ok, \
            count(*) FILTER (WHERE db.status IN ({failing}))::bigint AS failing, \
            count(*)::bigint AS total \
        FROM entry_point ep \
        LEFT JOIN build_job bj ON bj.evaluation = ep.evaluation AND bj.derivation = ep.derivation \
        LEFT JOIN derivation_build db ON db.id = bj.derivation_build \
        WHERE ep.evaluation = ANY($1::uuid[]) GROUP BY ep.evaluation",
        ok = crate::status_sql::build_in(&[Completed, Substituted]),
        failing = crate::status_sql::build_in(&[
            FailedPermanent,
            Aborted,
            DependencyFailed,
            FailedTimeout
        ]),
    )
}

fn history_sql() -> String {
    format!(
        "SELECT l.id AS latest, h.id, h.status, h.created_at, \
            (extract(epoch FROM (h.finished_at - h.created_at)) * 1000)::bigint AS duration_ms \
        FROM evaluation l CROSS JOIN LATERAL (SELECT e.id, e.status, e.created_at, e.finished_at \
            FROM evaluation e LEFT JOIN task_trigger tt ON tt.id = e.\"trigger\" \
            WHERE e.task = l.task AND {NON_PR} AND e.created_at <= l.created_at \
            ORDER BY e.created_at DESC LIMIT $2) h \
        WHERE l.id = ANY($1::uuid[]) ORDER BY l.id, h.created_at"
    )
}

crate::sql_fn! {
    TASK_FACTS = task_facts_sql,
        params = [UserId],
        tier = Bulk;

    ENTRY_POINT_OUTCOMES = outcomes_sql,
        params = [EvaluationIds(64)],
        tier = Bulk;

    HISTORIES = history_sql,
        params = [EvaluationIds(64), Int(30)],
        tier = Bulk;
}

#[derive(Clone, Debug, PartialEq)]
pub struct TaskFactsRow {
    pub project: String,
    pub task: String,
    pub starred: bool,
    pub latest: Option<(EvaluationId, EvaluationStatus, String, NaiveDateTime)>,
    pub previous: Option<EvaluationId>,
    pub recent_14d: i64,
    pub last_28d: i64,
    pub completed_30d: i64,
    pub failed_30d: i64,
    pub speed_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryRow {
    pub id: EvaluationId,
    pub status: EvaluationStatus,
    pub created_at: NaiveDateTime,
    pub duration_ms: Option<i64>,
}

fn eval_status(raw: i32) -> Result<EvaluationStatus, DbErr> {
    EvaluationStatus::try_from(raw).map_err(|e| DbErr::Type(e.to_string()))
}

fn uuids(ids: &[EvaluationId]) -> Value {
    ids.iter()
        .map(|i| i.into_inner())
        .collect::<Vec<Uuid>>()
        .into()
}

fn latest(
    r: &QueryResult,
) -> Result<Option<(EvaluationId, EvaluationStatus, String, NaiveDateTime)>, DbErr> {
    let Some(id) = r.try_get::<Option<Uuid>>("", "latest_id")? else {
        return Ok(None);
    };
    Ok(Some((
        EvaluationId::new(id),
        eval_status(r.try_get("", "latest_status")?)?,
        r.try_get::<Option<String>>("", "latest_commit")?
            .unwrap_or_default(),
        r.try_get("", "latest_created_at")?,
    )))
}

fn task_facts_row(r: &QueryResult) -> Result<TaskFactsRow, DbErr> {
    Ok(TaskFactsRow {
        project: r.try_get("", "project")?,
        task: r.try_get("", "task")?,
        starred: r.try_get("", "starred")?,
        latest: latest(r)?,
        previous: r
            .try_get::<Option<Uuid>>("", "previous_id")?
            .map(EvaluationId::new),
        recent_14d: r.try_get("", "recent_14d")?,
        last_28d: r.try_get("", "last_28d")?,
        completed_30d: r.try_get("", "completed_30d")?,
        failed_30d: r.try_get("", "failed_30d")?,
        speed_ms: r.try_get("", "speed_ms")?,
    })
}

/// Tasks of the viewer's projects plus starred tasks of public projects, PR evaluations excluded.
pub async fn task_facts<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<Vec<TaskFactsRow>, DbErr> {
    db.query_all_raw(TASK_FACTS.bind([Value::Uuid(Some(user.into_inner()))]))
        .await?
        .iter()
        .map(task_facts_row)
        .collect()
}

pub async fn entry_point_outcomes<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
) -> Result<HashMap<EvaluationId, (i64, i64, i64)>, DbErr> {
    let mut out = HashMap::with_capacity(evaluations.len());
    for chunk in evaluations.chunks(crate::IN_CHUNK_SIZE) {
        for r in db
            .query_all_raw(ENTRY_POINT_OUTCOMES.bind([uuids(chunk)]))
            .await?
        {
            let id = EvaluationId::new(r.try_get("", "id")?);
            out.insert(
                id,
                (
                    r.try_get("", "ok")?,
                    r.try_get("", "failing")?,
                    r.try_get("", "total")?,
                ),
            );
        }
    }
    Ok(out)
}

fn history_row(r: &QueryResult) -> Result<HistoryRow, DbErr> {
    Ok(HistoryRow {
        id: EvaluationId::new(r.try_get("", "id")?),
        status: eval_status(r.try_get("", "status")?)?,
        created_at: r.try_get("", "created_at")?,
        duration_ms: r.try_get("", "duration_ms")?,
    })
}

pub async fn histories<C: ConnectionTrait>(
    db: &C,
    latest: &[EvaluationId],
    n: u64,
) -> Result<HashMap<EvaluationId, Vec<HistoryRow>>, DbErr> {
    let mut out: HashMap<EvaluationId, Vec<HistoryRow>> = HashMap::new();
    for chunk in latest.chunks(crate::IN_CHUNK_SIZE) {
        let stmt = HISTORIES.bind([uuids(chunk), Value::BigInt(Some(n as i64))]);
        for r in db.query_all_raw(stmt).await? {
            out.entry(EvaluationId::new(r.try_get("", "latest")?))
                .or_default()
                .push(history_row(&r)?);
        }
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub cpu_time_ms: i64,
    pub cpu_time_ms_7d: i64,
    pub builds_completed: i64,
    pub queue_wait_p50_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ActivityDay {
    pub date: NaiveDate,
    pub evaluations: i64,
    pub failed: i64,
}

fn in_projects(column: &str, filter: Option<&str>) -> String {
    filter
        .map(|list| format!(" AND {column} IN ({list})"))
        .unwrap_or_default()
}

fn metrics_in_projects(filter: Option<&str>) -> String {
    filter
        .map(|list| {
            format!(
                " AND EXISTS (SELECT 1 FROM build_job bj JOIN evaluation e ON e.id = bj.evaluation \
                JOIN task t ON t.id = e.task WHERE bj.derivation = dm.derivation AND t.project IN ({list}))"
            )
        })
        .unwrap_or_default()
}

pub fn totals_sql(filter: Option<&str>) -> String {
    format!(
        "SELECT \
            coalesce(sum(dm.cpu_time_ms), 0)::bigint AS cpu_time_ms, \
            coalesce(sum(dm.cpu_time_ms) FILTER (WHERE dm.created_at > now() - interval '7 days'), 0)::bigint AS cpu_time_ms_7d, \
            count(*)::bigint AS builds_completed, \
            (SELECT coalesce((percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM (j.dispatched_at - j.queued_at)))) * 1000, 0)::bigint \
                FROM dispatched_job j WHERE j.dispatched_at > now() - interval '24 hours'{jobs}) AS queue_wait_p50_ms \
        FROM derivation_metric dm \
        WHERE true{metrics}",
        jobs = in_projects("j.project", filter),
        metrics = metrics_in_projects(filter),
    )
}

pub fn activity_sql(filter: Option<&str>) -> String {
    format!(
        "WITH per_day AS (SELECT e.created_at::date AS day, count(*)::bigint AS evaluations, \
            count(*) FILTER (WHERE e.status = {failed})::bigint AS failed \
            FROM evaluation e JOIN task t ON t.id = e.task LEFT JOIN task_trigger tt ON tt.id = e.\"trigger\" \
            WHERE e.created_at >= current_date - 370 AND {NON_PR}{scope} GROUP BY 1) \
        SELECT d::date AS date, coalesce(p.evaluations, 0) AS evaluations, coalesce(p.failed, 0) AS failed \
        FROM generate_series(current_date - 370, current_date, interval '1 day') d \
        LEFT JOIN per_day p ON p.day = d::date ORDER BY d",
        failed = crate::status_sql::eval(EvaluationStatus::Failed),
        scope = in_projects("t.project", filter),
    )
}

crate::sql_fn! {
    TOTALS = || totals_sql(Some("'11111111-1111-1111-1111-111111111111'")),
        params = [],
        tier = Bulk;

    ACTIVITY = || activity_sql(Some("'11111111-1111-1111-1111-111111111111'")),
        params = [],
        tier = Bulk;
}

crate::sql! {
    CACHE_SIZE = "SELECT coalesce(sum(cp.file_size), 0)::bigint AS bytes FROM cached_path cp \
        WHERE EXISTS (SELECT 1 FROM cached_path_signature s JOIN cache c ON c.id = s.cache \
        WHERE s.cached_path = cp.id AND ($2 OR c.created_by = $1 OR EXISTS (SELECT 1 FROM project_cache pc \
        JOIN project_user pu ON pu.project = pc.project WHERE pc.cache = c.id AND pu.\"user\" = $1)))",
        params = [UserId, Bool(false)],
        tier = Bulk;
}

pub async fn scoped_totals<C: ConnectionTrait>(
    db: &C,
    filter: Option<&str>,
) -> Result<Totals, DbErr> {
    let Some(r) = db
        .query_one_raw(TOTALS.bind_built(totals_sql(filter), []))
        .await?
    else {
        return Ok(Totals::default());
    };
    Ok(Totals {
        cpu_time_ms: r.try_get("", "cpu_time_ms")?,
        cpu_time_ms_7d: r.try_get("", "cpu_time_ms_7d")?,
        builds_completed: r.try_get("", "builds_completed")?,
        queue_wait_p50_ms: r.try_get("", "queue_wait_p50_ms")?,
    })
}

pub async fn cache_size<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    superuser: bool,
) -> Result<i64, DbErr> {
    let stmt = CACHE_SIZE.bind([
        Value::Uuid(Some(user.into_inner())),
        Value::Bool(Some(superuser)),
    ]);
    Ok(match db.query_one_raw(stmt).await? {
        Some(r) => r.try_get("", "bytes")?,
        None => 0,
    })
}

fn activity_day(r: &QueryResult) -> Result<ActivityDay, DbErr> {
    Ok(ActivityDay {
        date: r.try_get("", "date")?,
        evaluations: r.try_get("", "evaluations")?,
        failed: r.try_get("", "failed")?,
    })
}

pub async fn activity<C: ConnectionTrait>(
    db: &C,
    filter: Option<&str>,
) -> Result<Vec<ActivityDay>, DbErr> {
    db.query_all_raw(ACTIVITY.bind_built(activity_sql(filter), []))
        .await?
        .iter()
        .map(activity_day)
        .collect()
}
