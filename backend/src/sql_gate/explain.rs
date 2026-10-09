/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::disallowed_methods,
    reason = "the gate wraps a registered statement in EXPLAIN, which no registered statement can express"
)]

//! The measured pass is executing inside an always-rolled-back transaction. INSERT, UPDATE,
//! DELETE and FOR UPDATE are safe under EXPLAIN ANALYZE that way, since the statement is really
//! executing. Per-node timing is off because the VM's `acpi_pm` clock is trapping on every read. A
//! sweep over a million rows outlasted the statement timeout with it on.

use std::collections::HashMap;

use anyhow::{Context, Result};
use gradient_db::sql::{Flag, Query, check, measure, registry};
use sea_orm::sqlx::{AssertSqlSafe, Row, raw_sql};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DatabaseTransaction, Statement,
    TransactionTrait, Value,
};

use crate::report::Outcome;
use crate::sample::Sampler;

pub async fn run_all(
    db: &DatabaseConnection,
    mut sampler: Sampler,
) -> Result<Vec<(&'static Query, Outcome)>> {
    let relation_rows = relation_rows(db).await?;
    let mut queries: Vec<&'static Query> = registry().collect();
    queries.sort_by_key(|query| (query.file, query.line));

    let mut rows = Vec::with_capacity(queries.len());
    for query in queries {
        eprintln!("measuring {} ({})", query.name, query.location());
        let outcome = run_one(db, query, &relation_rows, &mut sampler).await?;
        rows.push((query, outcome));
    }

    Ok(rows)
}

async fn run_one(
    db: &DatabaseConnection,
    query: &'static Query,
    relation_rows: &HashMap<String, u64>,
    sampler: &mut Sampler,
) -> Result<Outcome> {
    let sql = query.text();

    let generic = match plan(db, format!("EXPLAIN (GENERIC_PLAN, FORMAT JSON) {sql}")).await {
        Ok(plan) => plan,
        Err(err) => return Ok(Outcome::Unmeasured(format!("generic plan failed: {err}"))),
    };

    for relation in relations(&generic) {
        if relation_rows.get(&relation).copied().unwrap_or_default() == 0 {
            return Ok(Outcome::Unmeasured(format!("{relation} has 0 rows")));
        }
    }

    let mut values = Vec::with_capacity(query.params.len());
    for param in query.params {
        match sampler.value(db, param).await {
            Ok(Some(value)) => values.push(value),
            Ok(None) => return Ok(Outcome::Unmeasured(format!("no {param:?} to draw"))),
            Err(err) => {
                return Ok(Outcome::Unmeasured(format!(
                    "drawing {param:?} failed: {err}"
                )));
            }
        }
    }

    let inputs = align_array_widths(&mut values);

    let txn = db.begin().await?;
    let measured = measure_in(&txn, query, &sql, values).await;
    txn.rollback().await?;

    let mut measured = match measured {
        Ok(plan) => measure(&plan).map_err(|err| anyhow::anyhow!("{err}"))?,
        Err(err) => return Ok(Outcome::Unmeasured(format!("explain failed: {err}"))),
    };

    measured.inputs = inputs as u64;

    let violations = check(&measured, &query.budget, relation_rows);
    Ok(if violations.is_empty() {
        Outcome::Pass(measured)
    } else {
        Outcome::Fail(violations)
    })
}

async fn measure_in(
    txn: &DatabaseTransaction,
    query: &Query,
    sql: &str,
    values: Vec<Value>,
) -> Result<serde_json::Value> {
    if query.flags.contains(&Flag::Walk) {
        txn.execute_unprepared(gradient_db::graph::walks::work_mem(query.tier))
            .await?;
    }

    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        format!("EXPLAIN (ANALYZE, TIMING OFF, BUFFERS, FORMAT JSON) {sql}"),
        values,
    );

    txn.query_one_raw(stmt)
        .await?
        .context("EXPLAIN returned no row")?
        .try_get::<serde_json::Value>("", "QUERY PLAN")
        .map_err(Into::into)
}

fn align_array_widths(values: &mut [Value]) -> usize {
    let Some(width) = values.iter().filter_map(array_len).min() else {
        return 0;
    };

    for value in values {
        if let Value::Array(_, Some(items)) = value {
            items.truncate(width);
        }
    }

    width
}

fn array_len(value: &Value) -> Option<usize> {
    match value {
        Value::Array(_, Some(items)) => Some(items.len()),
        _ => None,
    }
}

/// `GENERIC_PLAN` is needing unbound parameters, which the extended protocol cannot express.
/// sea-orm is preparing every statement, and Postgres is refusing the bind without the declared
/// `$n`. The simple protocol is the only way to ask for this plan.
async fn plan(db: &DatabaseConnection, sql: String) -> Result<serde_json::Value> {
    let mut conn = db.get_postgres_connection_pool().acquire().await?;
    let row = raw_sql(AssertSqlSafe(sql)).fetch_one(&mut *conn).await?;

    row.try_get::<serde_json::Value, _>(0)
        .context("EXPLAIN returned no plan")
}

fn relations(plan: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();

    if let Some(root) = plan.get(0).and_then(|entry| entry.get("Plan")) {
        collect(root, &mut out);
    }

    out
}

fn collect(node: &serde_json::Value, out: &mut Vec<String>) {
    if let Some(relation) = node
        .get("Relation Name")
        .and_then(serde_json::Value::as_str)
    {
        out.push(relation.to_string());
    }

    if let Some(children) = node.get("Plans").and_then(serde_json::Value::as_array) {
        for child in children {
            collect(child, out);
        }
    }
}

async fn relation_rows(db: &DatabaseConnection) -> Result<HashMap<String, u64>> {
    let rows = db
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT c.relname AS relname, GREATEST(c.reltuples, 0)::bigint AS rows \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'public' AND c.relkind = 'r'",
        ))
        .await?;

    let mut out = HashMap::with_capacity(rows.len());
    for row in rows {
        out.insert(
            row.try_get::<String>("", "relname")?,
            row.try_get::<i64>("", "rows")? as u64,
        );
    }

    Ok(out)
}
