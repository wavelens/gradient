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
    use gradient_fixtures::mock::{exec_ok, mock_db, raw_statements};
    use sea_orm::sea_query::ValueType;
    use sea_orm::{DatabaseConnection, Statement};

    fn path(index: usize) -> Vec<String> {
        vec!["packages".into(), format!("child{index}")]
    }

    fn accepting_db() -> DatabaseConnection {
        mock_db()
            .append_exec_results((0..16).map(|_| exec_ok()))
            .into_connection()
    }

    async fn recorded(sets: &[EvalSetCost], subtrees: &[EvalSubtreeCost]) -> Vec<Statement> {
        let db = accepting_db();
        record_eval_costs(&db, TaskId::now_v7(), sets, subtrees)
            .await
            .expect("record");
        raw_statements(db.into_transaction_log())
    }

    fn starting_with<'a>(statements: &'a [Statement], prefix: &str) -> Vec<&'a Statement> {
        statements
            .iter()
            .filter(|statement| statement.sql.starts_with(prefix))
            .collect()
    }

    #[tokio::test]
    async fn costs_of_many_subtrees_take_one_statement_per_chunk() {
        let sets: Vec<EvalSetCost> = (0..3)
            .map(|index| EvalSetCost {
                path: path(index),
                startup_ms: Some(100),
                peak_rss_mb: 512,
            })
            .collect();
        let subtrees: Vec<EvalSubtreeCost> = (0..2500)
            .map(|index| EvalSubtreeCost {
                path: path(index),
                attrs: 2,
                eval_ms: 30,
            })
            .collect();

        let statements = recorded(&sets, &subtrees).await;

        assert_eq!(statements.len(), 6, "{statements:?}");
        assert_eq!(
            starting_with(&statements, "INSERT INTO \"eval_subtree_cost\"").len(),
            3
        );
        assert_eq!(
            starting_with(&statements, "INSERT INTO \"eval_set_cost\"").len(),
            1
        );
        assert_eq!(starting_with(&statements, "DELETE FROM").len(), 2);
    }

    #[tokio::test]
    async fn a_missing_startup_keeps_the_stored_value() {
        let sets = [EvalSetCost {
            path: path(0),
            startup_ms: None,
            peak_rss_mb: 512,
        }];

        let statements = recorded(&sets, &[]).await;

        let upsert = starting_with(&statements, "INSERT INTO \"eval_set_cost\"")[0];
        assert!(
            upsert.sql.contains(
                "\"startup_ms\" = coalesce(\"eval_set_cost\".\"startup_ms\" * 0.5 + \
                 \"excluded\".\"startup_ms\" * 0.5, \"eval_set_cost\".\"startup_ms\", \
                 \"excluded\".\"startup_ms\")"
            ),
            "{}",
            upsert.sql
        );
        let values = format!("{:?}", upsert.values);
        assert!(values.contains("Double(None)"), "{values}");
    }

    #[tokio::test]
    async fn the_seat_peak_keeps_the_largest_value() {
        let sets = [EvalSetCost {
            path: path(0),
            startup_ms: None,
            peak_rss_mb: 900,
        }];

        let statements = recorded(&sets, &[]).await;

        let upsert = starting_with(&statements, "INSERT INTO \"eval_set_cost\"")[0];
        assert!(
            upsert.sql.contains(
                "\"max_peak_rss_mb\" = greatest(\"eval_set_cost\".\"max_peak_rss_mb\", \
                 \"excluded\".\"max_peak_rss_mb\")"
            ),
            "{}",
            upsert.sql
        );
        assert!(
            upsert.sql.contains(
                "\"peak_rss_mb\" = \"eval_set_cost\".\"peak_rss_mb\" * 0.5 + \
                 \"excluded\".\"peak_rss_mb\" * 0.5"
            ),
            "{}",
            upsert.sql
        );
        let values = format!("{:?}", upsert.values);
        assert_eq!(values.matches("Double(Some(900.0))").count(), 2, "{values}");
    }

    #[tokio::test]
    async fn stale_estimates_are_deleted() {
        let task = TaskId::now_v7();
        let db = accepting_db();
        let before = gradient_types::now();

        record_eval_costs(&db, task, &[], &[])
            .await
            .expect("record");
        let after = gradient_types::now();

        let statements = raw_statements(db.into_transaction_log());
        let deletes = starting_with(&statements, "DELETE FROM");
        assert_eq!(deletes.len(), 2);
        for delete in deletes {
            assert!(delete.sql.contains("\"task\" = $1"), "{}", delete.sql);
            assert!(delete.sql.contains("\"updated_at\" < $2"), "{}", delete.sql);

            let values = delete.values.as_ref().expect("bound values");
            assert_eq!(values.0[0], sea_orm::Value::from(task.into_inner()));
            let cutoff = <NaiveDateTime as ValueType>::try_from(values.0[1].clone())
                .expect("a timestamp cutoff");
            assert!(
                before - STALE_AFTER <= cutoff && cutoff <= after - STALE_AFTER,
                "{cutoff}"
            );
        }
    }
}
