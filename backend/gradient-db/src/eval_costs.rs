/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::{NaiveDateTime, TimeDelta};
use gradient_entity::eval_set_cost::{Column as CSetCost, Entity as ESetCost, Model as MSetCost};
use gradient_entity::eval_subtree_cost::{
    Column as CSubtreeCost, Entity as ESubtreeCost, Model as MSubtreeCost,
};
use gradient_entity::ids::{EvalSetCostId, EvalSubtreeCostId, TaskId};
use gradient_wire::types::{EvalSetCost, EvalSubtreeCost};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, QueryFilter,
    TransactionSession, TransactionTrait,
};

use crate::chunked::insert_in_chunks;

pub const ESTIMATE_WEIGHT: f64 = 0.5;
pub const STALE_AFTER: TimeDelta = TimeDelta::days(30);
pub const ROWS_PER_STATEMENT: usize = 1000;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SetCost {
    pub path: Vec<String>,
    pub startup_ms: Option<f64>,
    pub peak_rss_mb: f64,
    pub max_peak_rss_mb: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SubtreeCost {
    pub path: Vec<String>,
    pub attrs: f64,
    pub ms_per_attr: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TaskEvalCosts {
    pub sets: Vec<SetCost>,
    pub subtrees: Vec<SubtreeCost>,
}

pub async fn record_eval_costs<C: ConnectionTrait + TransactionTrait>(
    db: &C,
    task: TaskId,
    sets: &[EvalSetCost],
    subtrees: &[EvalSubtreeCost],
) -> Result<(), DbErr> {
    let now = gradient_types::now();
    let txn = db.begin().await?;
    let sets = set_rows(task, sets, now);
    insert_in_chunks(&txn, sets, ROWS_PER_STATEMENT, Some(set_conflict())).await?;
    let subtrees = subtree_rows(task, subtrees, now);
    insert_in_chunks(&txn, subtrees, ROWS_PER_STATEMENT, Some(subtree_conflict())).await?;
    delete_stale(&txn, task, now).await?;
    txn.commit().await
}

pub async fn load_eval_costs<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
) -> Result<TaskEvalCosts, DbErr> {
    let sets = ESetCost::find()
        .filter(CSetCost::Task.eq(task))
        .all(db)
        .await?;
    let subtrees = ESubtreeCost::find()
        .filter(CSubtreeCost::Task.eq(task))
        .all(db)
        .await?;

    Ok(TaskEvalCosts {
        sets: sets
            .into_iter()
            .map(|row| SetCost {
                path: row.path,
                startup_ms: row.startup_ms,
                peak_rss_mb: row.peak_rss_mb,
                max_peak_rss_mb: row.max_peak_rss_mb,
            })
            .collect(),
        subtrees: subtrees
            .into_iter()
            .map(|row| SubtreeCost {
                path: row.path,
                attrs: row.attrs,
                ms_per_attr: row.ms_per_attr,
            })
            .collect(),
    })
}

fn set_rows(
    task: TaskId,
    sets: &[EvalSetCost],
    now: NaiveDateTime,
) -> Vec<gradient_entity::eval_set_cost::ActiveModel> {
    let rows = sets.iter().map(|set| MSetCost {
        id: EvalSetCostId::now_v7(),
        task,
        path: set.path.clone(),
        startup_ms: set.startup_ms.map(|ms| ms as f64),
        peak_rss_mb: set.peak_rss_mb as f64,
        max_peak_rss_mb: set.peak_rss_mb as f64,
        samples: 1,
        updated_at: now,
    });

    unique_paths(rows.collect(), |row| &row.path)
        .into_iter()
        .map(IntoActiveModel::into_active_model)
        .collect()
}

fn subtree_rows(
    task: TaskId,
    subtrees: &[EvalSubtreeCost],
    now: NaiveDateTime,
) -> Vec<gradient_entity::eval_subtree_cost::ActiveModel> {
    let rows = subtrees
        .iter()
        .filter(|subtree| subtree.attrs > 0)
        .map(|subtree| MSubtreeCost {
            id: EvalSubtreeCostId::now_v7(),
            task,
            path: subtree.path.clone(),
            attrs: subtree.attrs as f64,
            ms_per_attr: subtree.eval_ms as f64 / subtree.attrs as f64,
            samples: 1,
            updated_at: now,
        });

    unique_paths(rows.collect(), |row| &row.path)
        .into_iter()
        .map(IntoActiveModel::into_active_model)
        .collect()
}

fn unique_paths<T>(mut rows: Vec<T>, path: impl Fn(&T) -> &Vec<String>) -> Vec<T> {
    rows.sort_by(|a, b| path(a).cmp(path(b)));
    rows.dedup_by(|a, b| path(a) == path(b));
    rows
}

fn moving_average(table: &str, column: &str) -> Expr {
    Expr::cust(average_sql(table, column))
}

fn average_sql(table: &str, column: &str) -> String {
    let keep = 1.0 - ESTIMATE_WEIGHT;
    format!("\"{table}\".\"{column}\" * {keep:?} + \"excluded\".\"{column}\" * {ESTIMATE_WEIGHT:?}")
}

fn one_more_sample(table: &str) -> Expr {
    Expr::cust(format!("\"{table}\".\"samples\" + 1"))
}

fn set_conflict() -> OnConflict {
    let table = "eval_set_cost";
    OnConflict::columns([CSetCost::Task, CSetCost::Path])
        .value(
            CSetCost::StartupMs,
            Expr::cust(format!(
                "coalesce({}, \"{table}\".\"startup_ms\", \"excluded\".\"startup_ms\")",
                average_sql(table, "startup_ms")
            )),
        )
        .value(CSetCost::PeakRssMb, moving_average(table, "peak_rss_mb"))
        .value(
            CSetCost::MaxPeakRssMb,
            Expr::cust(format!(
                "greatest(\"{table}\".\"max_peak_rss_mb\", \"excluded\".\"max_peak_rss_mb\")"
            )),
        )
        .value(CSetCost::Samples, one_more_sample(table))
        .update_column(CSetCost::UpdatedAt)
        .to_owned()
}

fn subtree_conflict() -> OnConflict {
    let table = "eval_subtree_cost";
    OnConflict::columns([CSubtreeCost::Task, CSubtreeCost::Path])
        .value(CSubtreeCost::Attrs, moving_average(table, "attrs"))
        .value(
            CSubtreeCost::MsPerAttr,
            moving_average(table, "ms_per_attr"),
        )
        .value(CSubtreeCost::Samples, one_more_sample(table))
        .update_column(CSubtreeCost::UpdatedAt)
        .to_owned()
}

async fn delete_stale(
    db: &impl ConnectionTrait,
    task: TaskId,
    now: NaiveDateTime,
) -> Result<(), DbErr> {
    let cutoff = now - STALE_AFTER;
    ESetCost::delete_many()
        .filter(CSetCost::Task.eq(task))
        .filter(CSetCost::UpdatedAt.lt(cutoff))
        .exec(db)
        .await?;
    ESubtreeCost::delete_many()
        .filter(CSubtreeCost::Task.eq(task))
        .filter(CSubtreeCost::UpdatedAt.lt(cutoff))
        .exec(db)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::ids::EvaluationId;

    fn path(index: usize) -> Vec<String> {
        vec!["packages".into(), format!("child{index}")]
    }

    fn set(index: usize, startup_ms: Option<u64>, peak_rss_mb: u64) -> EvalSetCost {
        EvalSetCost {
            path: path(index),
            startup_ms,
            peak_rss_mb,
        }
    }

    #[tokio::test]
    async fn test_record_eval_costs_averages_values_keeps_largest_peak_and_stored_startup_chunks_many_rows_and_deletes_only_stale_rows_of_same_task()
     {
        let [first, second] = [(); 2].map(|_| EvaluationId::now_v7());
        let db = gradient_fixtures::shape::shape()
            .evaluation(first)
            .evaluation(second)
            .into_database()
            .await;
        let conn = db.connection();
        let (task, other_task) = (db.task_of(first).await, db.task_of(second).await);
        let many_subtrees: Vec<EvalSubtreeCost> = (0..2501)
            .map(|index| EvalSubtreeCost {
                path: path(index),
                attrs: if index == 2500 { 0 } else { 2 },
                eval_ms: 30,
            })
            .collect();

        for owner in [task, other_task] {
            record_eval_costs(
                &conn,
                owner,
                &[set(0, Some(100), 512), set(1, None, 100)],
                &many_subtrees,
            )
            .await
            .unwrap();
        }
        db.execute(&format!(
            "UPDATE eval_set_cost SET updated_at = updated_at - interval '40 days' \
             WHERE task = '{other_task}' OR (task = '{task}' AND path = ARRAY['packages','child1'])"
        ))
        .await;
        db.execute(&format!(
            "UPDATE eval_subtree_cost SET updated_at = updated_at - interval '40 days' \
             WHERE task = '{other_task}' OR (task = '{task}' AND path = ARRAY['packages','child0'])"
        ))
        .await;
        record_eval_costs(&conn, task, &[set(0, None, 900)], &[])
            .await
            .unwrap();
        record_eval_costs(&conn, task, &[set(0, Some(200), 100)], &[])
            .await
            .unwrap();

        let costs = load_eval_costs(&conn, task).await.unwrap();
        let other = load_eval_costs(&conn, other_task).await.unwrap();
        let sets = db.rows::<ESetCost>().await;
        let samples = sets
            .iter()
            .find(|row| row.task == task)
            .map(|row| row.samples);
        db.finish().await;

        assert_eq!(
            costs.sets,
            vec![SetCost {
                path: path(0),
                startup_ms: Some(150.0),
                peak_rss_mb: 403.0,
                max_peak_rss_mb: 900.0,
            }]
        );
        assert_eq!(samples, Some(3));
        assert_eq!(costs.subtrees.len(), 2499);
        assert!(costs.subtrees.iter().all(|s| s.path != path(0)));
        assert!(costs.subtrees.iter().all(|s| s.path != path(2500)));
        assert_eq!(
            (other.sets.len(), other.subtrees.len()),
            (2, 2500),
            "the other task keeps its stale rows"
        );
    }
}
