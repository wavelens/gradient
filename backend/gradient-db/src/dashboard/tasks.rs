/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::NON_PR;
use super::stars::user_value;
use chrono::NaiveDateTime;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, QueryResult, Value};
use std::collections::HashMap;
use uuid::Uuid;

fn terminal() -> String {
    crate::sql::status::eval_in(&EvaluationStatus::TERMINAL)
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
            coalesce(a.recent_14d, 0) AS recent_14d, sp.speed_ms \
        FROM viewer_tasks v JOIN task t ON t.id = v.id JOIN project p ON p.id = t.project \
        LEFT JOIN LATERAL (SELECT e.id, e.status, e.commit, e.created_at FROM evaluation e \
            LEFT JOIN task_trigger tt ON tt.id = e.\"trigger\" WHERE e.task = t.id AND {NON_PR} \
            ORDER BY e.created_at DESC LIMIT 1) l ON true \
        LEFT JOIN commit c ON c.id = l.commit \
        LEFT JOIN LATERAL (SELECT e.id FROM evaluation e LEFT JOIN task_trigger tt ON tt.id = e.\"trigger\" \
            WHERE e.task = t.id AND {NON_PR} AND e.status IN ({term}) AND e.created_at < l.created_at \
            ORDER BY e.created_at DESC LIMIT 1) pv ON true \
        LEFT JOIN LATERAL (SELECT count(*) AS recent_14d \
            FROM evaluation e LEFT JOIN task_trigger tt ON tt.id = e.\"trigger\" \
            WHERE e.task = t.id AND {NON_PR} AND e.created_at > now() - interval '14 days') a ON true \
        LEFT JOIN LATERAL (SELECT (avg(extract(epoch FROM (r.finished_at - r.created_at))) * 1000)::bigint AS speed_ms \
            FROM (SELECT e.created_at, e.finished_at FROM evaluation e LEFT JOIN task_trigger tt ON tt.id = e.\"trigger\" \
                WHERE e.task = t.id AND {NON_PR} AND e.status IN ({term}) AND e.finished_at IS NOT NULL \
                ORDER BY e.created_at DESC LIMIT 30) r) sp ON true",
        term = terminal(),
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
        ok = crate::sql::status::build_in(&[Completed, Substituted]),
        failing = crate::sql::status::build_in(&[
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
        WHERE l.id = ANY($1::uuid[]) ORDER BY h.created_at"
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
    pub speed_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryRow {
    pub id: EvaluationId,
    pub status: EvaluationStatus,
    pub created_at: NaiveDateTime,
    pub duration_ms: Option<i64>,
}

pub(super) fn eval_status(raw: i32) -> Result<EvaluationStatus, DbErr> {
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
        speed_ms: r.try_get("", "speed_ms")?,
    })
}

pub async fn task_facts<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<Vec<TaskFactsRow>, DbErr> {
    db.query_all_raw(TASK_FACTS.bind([user_value(user)]))
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
