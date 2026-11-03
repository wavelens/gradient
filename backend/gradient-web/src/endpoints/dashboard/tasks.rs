/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::rank::{
    Counts, Filter, HistoryBar, Outcomes, Paging, TaskFacts, TaskRow, counts, history_len, rank,
};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use axum::extract::{Query, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::dashboard::{entry_point_outcomes, histories, task_facts};
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TasksQuery {
    pub filter: Option<Filter>,
    pub page: Option<u64>,
    pub per_page: Option<u64>,
    pub history: Option<u64>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct TasksPage {
    pub filter: Filter,
    pub counts: Counts,
    pub total: usize,
    pub tasks: Vec<TaskRow>,
}

async fn outcomes_for<C: ConnectionTrait>(
    db: &C,
    facts: &[TaskFacts],
) -> Result<HashMap<EvaluationId, Outcomes>, DbErr> {
    let ids: Vec<EvaluationId> = facts
        .iter()
        .flat_map(|f| {
            f.latest
                .as_ref()
                .map(|l| l.id)
                .into_iter()
                .chain(f.previous)
        })
        .collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut outcomes: HashMap<EvaluationId, Outcomes> =
        ids.iter().map(|id| (*id, Outcomes::default())).collect();
    outcomes.extend(
        entry_point_outcomes(db, &ids)
            .await?
            .into_iter()
            .map(|(id, (ok, failing, total))| (id, Outcomes { ok, failing, total })),
    );
    Ok(outcomes)
}

async fn attach_history<C: ConnectionTrait>(
    db: &C,
    rows: &mut [TaskRow],
    n: u64,
) -> Result<(), DbErr> {
    let latest: Vec<EvaluationId> = rows
        .iter()
        .filter_map(|r| r.latest.as_ref().map(|l| l.id))
        .collect();
    if latest.is_empty() {
        return Ok(());
    }
    let mut by_latest = histories(db, &latest, n).await?;
    for row in rows.iter_mut() {
        if let Some(l) = &row.latest {
            row.history = by_latest
                .remove(&l.id)
                .unwrap_or_default()
                .into_iter()
                .map(HistoryBar::from)
                .collect();
        }
    }
    Ok(())
}

pub async fn load_page<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    filter: Option<Filter>,
    paging: Paging,
    history: u64,
) -> Result<TasksPage, DbErr> {
    let facts: Vec<TaskFacts> = task_facts(db, user)
        .await?
        .into_iter()
        .map(TaskFacts::from)
        .collect();
    let outcomes = outcomes_for(db, &facts).await?;
    let mut rows: Vec<TaskRow> = facts
        .into_iter()
        .map(|f| TaskRow::build(f, &outcomes))
        .collect();
    rank(&mut rows);
    let counts = counts(&rows);
    let filter = filter.unwrap_or_else(|| counts.default_filter());
    let matching: Vec<TaskRow> = rows.into_iter().filter(|r| r.matches(filter)).collect();
    let total = matching.len();
    let mut page: Vec<TaskRow> = matching
        .into_iter()
        .skip(paging.offset)
        .take(paging.limit)
        .collect();
    attach_history(db, &mut page, history).await?;
    Ok(TasksPage {
        filter,
        counts,
        total,
        tasks: page,
    })
}

#[utoipa::path(
    get,
    path = "/dashboard/tasks",
    operation_id = "getDashboardTasks",
    tag = "dashboard",
    summary = "Ranked task table of the current user",
    description = "Tasks of the viewer's projects plus starred tasks of public projects, PR-triggered evaluations excluded.\nRanked by tier (`starred_active`, `active`, `starred`, `team`, `member`), then newest latest evaluation first.\nActive means a non-PR evaluation in the last 14 days. `counts` always covers every task; `total` is the number of tasks matching `filter`.",
    params(TasksQuery),
    responses((status = OK, description = "One page of ranked tasks", body = TasksPage), WebError)
)]
pub async fn get_tasks(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Query(q): Query<TasksQuery>,
) -> WebResult<Json<BaseResponse<TasksPage>>> {
    let paging = Paging::from_query(q.page, q.per_page);
    Ok(ok_json(
        load_page(
            &state.web_db,
            user.id,
            q.filter,
            paging,
            history_len(q.history),
        )
        .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::mock_db;
    use sea_orm::Value;
    use std::collections::BTreeMap;

    fn task_row(
        latest: Option<(uuid::Uuid, i32)>,
        previous: Option<uuid::Uuid>,
    ) -> BTreeMap<&'static str, Value> {
        let created_at = chrono::NaiveDate::from_ymd_opt(2026, 9, 20)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        BTreeMap::from([
            ("project", Value::from("p")),
            ("task", Value::from("t")),
            ("starred", Value::from(true)),
            ("team", Value::Bool(Some(false))),
            ("latest_id", Value::Uuid(latest.map(|(id, _)| id))),
            ("latest_status", Value::Int(latest.map(|(_, s)| s))),
            (
                "latest_commit",
                Value::String(latest.map(|_| "abc".to_string())),
            ),
            (
                "latest_created_at",
                Value::ChronoDateTime(latest.map(|_| created_at)),
            ),
            ("previous_id", Value::Uuid(previous)),
            ("recent_14d", Value::BigInt(Some(0))),
            ("speed_ms", Value::BigInt(None)),
        ])
    }

    #[tokio::test]
    async fn no_tasks_asks_nothing_more() {
        let db = mock_db()
            .append_query_results([Vec::<BTreeMap<&str, Value>>::new()])
            .into_connection();

        let page = load_page(
            &db,
            UserId::now_v7(),
            None,
            Paging::from_query(None, None),
            30,
        )
        .await
        .unwrap();

        assert_eq!(page.filter, Filter::All);
        assert_eq!(page.total, 0);
        assert_eq!(page.counts, Counts::default());
        assert!(page.tasks.is_empty());
        assert_eq!(db.into_transaction_log().len(), 1);
    }

    #[tokio::test]
    async fn a_page_past_the_end_keeps_the_counts() {
        let db = mock_db()
            .append_query_results([vec![task_row(None, None)]])
            .into_connection();

        let page = load_page(
            &db,
            UserId::now_v7(),
            Some(Filter::All),
            Paging::from_query(Some(9), None),
            30,
        )
        .await
        .unwrap();

        assert_eq!(page.total, 1);
        assert_eq!(page.counts.starred, 1);
        assert!(page.tasks.is_empty());
        assert_eq!(db.into_transaction_log().len(), 1);
    }

    #[tokio::test]
    async fn no_filter_picks_starred_once_a_task_is_starred() {
        let db = mock_db()
            .append_query_results([vec![task_row(None, None)]])
            .into_connection();

        let page = load_page(
            &db,
            UserId::now_v7(),
            None,
            Paging::from_query(None, None),
            30,
        )
        .await
        .unwrap();

        assert_eq!(page.filter, Filter::Starred);
        assert_eq!(page.total, 1);
    }

    #[tokio::test]
    async fn evaluations_without_entry_points_count_as_zero() {
        let completed = i32::from(gradient_entity::evaluation::EvaluationStatus::Completed);
        let task = task_row(
            Some((uuid::Uuid::now_v7(), completed)),
            Some(uuid::Uuid::now_v7()),
        );
        let db = mock_db()
            .append_query_results([
                vec![task],
                Vec::<BTreeMap<&str, Value>>::new(),
                Vec::<BTreeMap<&str, Value>>::new(),
            ])
            .into_connection();

        let page = load_page(
            &db,
            UserId::now_v7(),
            Some(Filter::All),
            Paging::from_query(None, None),
            30,
        )
        .await
        .unwrap();

        let row = &page.tasks[0];
        assert_eq!(row.delta, Some(0));
        assert_eq!(row.entry_points, Some(Outcomes::default()));
        assert!(row.history.is_empty());
    }
}
