/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, Value};
use std::collections::HashMap;
use uuid::Uuid;

pub(crate) const NON_PR: &str = "tt.trigger_type IS DISTINCT FROM 2";

const HEAD_WINDOW: u32 = 50;

/// Select the current evaluation for a task. The branch head commit is the commit with the newest first run. Prefer the latest run of that commit. Only the most recent 50 evaluations count. Pull request runs never count. A manual rerun of an older commit can never replace the branch head.
pub(crate) fn task_current_evaluation_subquery_sql(task: &str) -> String {
    format!(
        "SELECT r.id, r.status, r.commit, r.created_at, r.waiting_reason \
        FROM (SELECT e.id, e.status, e.commit, e.created_at, e.waiting_reason FROM evaluation e \
            LEFT JOIN task_trigger tt ON tt.id = e.\"trigger\" WHERE e.task = {task} AND {NON_PR} \
            ORDER BY e.created_at DESC LIMIT {HEAD_WINDOW}) r \
        ORDER BY (SELECT min(f.created_at) FROM evaluation f WHERE f.task = {task} AND f.commit = r.commit) DESC, \
            r.created_at DESC LIMIT 1"
    )
}

fn current_task_evaluations_sql() -> String {
    format!(
        "SELECT t.id AS task, l.id, l.status FROM unnest($1::uuid[]) AS t(id) \
        CROSS JOIN LATERAL ({}) l",
        task_current_evaluation_subquery_sql("t.id")
    )
}

crate::sql_fn! {
    CURRENT_TASK_EVALUATIONS = current_task_evaluations_sql,
        params = [TaskIds(64)],
        tier = Bulk;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LatestEvaluation {
    pub id: EvaluationId,
    pub status: EvaluationStatus,
}

pub async fn latest_evaluation_status_of_tasks<C: ConnectionTrait>(
    db: &C,
    tasks: &[TaskId],
) -> Result<HashMap<TaskId, LatestEvaluation>, DbErr> {
    let mut out = HashMap::with_capacity(tasks.len());
    for chunk in tasks.chunks(crate::IN_CHUNK_SIZE) {
        let ids: Value = chunk
            .iter()
            .map(|task| task.into_inner())
            .collect::<Vec<Uuid>>()
            .into();
        for row in db
            .query_all_raw(CURRENT_TASK_EVALUATIONS.bind([ids]))
            .await?
        {
            let status = EvaluationStatus::try_from(row.try_get::<i32>("", "status")?)
                .map_err(|e| DbErr::Type(e.to_string()))?;
            out.insert(
                TaskId::new(row.try_get("", "task")?),
                LatestEvaluation {
                    id: EvaluationId::new(row.try_get("", "id")?),
                    status,
                },
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, sea_query::Expr};

    #[tokio::test]
    async fn rerun_of_older_commit_keeps_branch_head_as_current_evaluation() {
        let (old, head, rerun) = (
            EvaluationId::now_v7(),
            EvaluationId::now_v7(),
            EvaluationId::now_v7(),
        );
        let db = gradient_fixtures::shape::shape()
            .evaluation(old)
            .evaluation_status(EvaluationStatus::Failed)
            .evaluation(head)
            .same_task_as(old)
            .evaluation_status(EvaluationStatus::Completed)
            .evaluation(rerun)
            .same_task_as(old)
            .evaluation_status(EvaluationStatus::Failed)
            .into_database()
            .await;
        let conn = db.connection();
        for (evaluation, hours_ago) in [(old, 2), (head, 1)] {
            EEvaluation::update_many()
                .col_expr(
                    CEvaluation::CreatedAt,
                    Expr::cust(format!("created_at - interval '{hours_ago} hour'")),
                )
                .filter(CEvaluation::Id.eq(evaluation))
                .exec(&conn)
                .await
                .expect("the evaluation is moved back");
        }
        let old_run = db.find::<EEvaluation>(old).await.expect("the old run");
        EEvaluation::update_many()
            .col_expr(CEvaluation::Commit, Expr::value(old_run.commit))
            .filter(CEvaluation::Id.eq(rerun))
            .exec(&conn)
            .await
            .expect("the rerun points at the old commit");
        let task = old_run.task.expect("the run belongs to a task");

        let current = latest_evaluation_status_of_tasks(&conn, &[task])
            .await
            .expect("the query runs");

        assert_eq!(
            current,
            HashMap::from([(
                task,
                LatestEvaluation {
                    id: head,
                    status: EvaluationStatus::Completed
                }
            )])
        );
        db.finish().await;
    }
}
