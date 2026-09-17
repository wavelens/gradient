/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::disallowed_methods,
    reason = "the gate wraps a registered statement in EXPLAIN, which no registered statement can express"
)]

//! Runs one registered query through the planner. `EXPLAIN (GENERIC_PLAN)` comes
//! first because it plans without values and names the relations: a query whose
//! relations are empty is unmeasured, and executing it would prove nothing. The
//! measured pass then runs inside a transaction that is always rolled back,
//! which is what makes an INSERT, UPDATE, DELETE or FOR UPDATE safe to
//! EXPLAIN ANALYZE: the statement really does execute.

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

const STATEMENT_TIMEOUT: &str = "SET LOCAL statement_timeout = '30s'";
const LOCK_TIMEOUT: &str = "SET LOCAL lock_timeout = '2s'";
const WALK_WORK_MEM: &str = "SET LOCAL work_mem = '64MB'";

pub async fn run_all(db: &DatabaseConnection) -> Result<Vec<(&'static Query, Outcome)>> {
    db.execute_unprepared("ANALYZE")
        .await
        .context("ANALYZE before measuring")?;

    let relation_rows = relation_rows(db).await?;
    let mut sampler = Sampler::default();
    let mut queries: Vec<&'static Query> = registry().collect();
    queries.sort_by_key(|query| (query.file, query.line));

    let mut rows = Vec::with_capacity(queries.len());
    for query in queries {
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
        match sampler.value(db, param).await? {
            Some(value) => values.push(value),
            None => return Ok(Outcome::Unmeasured(format!("no {param:?} to draw"))),
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
    txn.execute_unprepared(STATEMENT_TIMEOUT).await?;
    txn.execute_unprepared(LOCK_TIMEOUT).await?;

    if query.flags.contains(&Flag::Walk) {
        txn.execute_unprepared(WALK_WORK_MEM).await?;
    }

    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {sql}"),
        values,
    );

    txn.query_one_raw(stmt)
        .await?
        .context("EXPLAIN returned no row")?
        .try_get::<serde_json::Value>("", "QUERY PLAN")
        .map_err(Into::into)
}

/// `unnest($1, $2, ...)` pads the shorter arrays with NULL and a NOT NULL column
/// then rejects the row, so every array a statement binds is cut to the shortest
/// one drawn: a table with fewer rows than the declared width decides the width
/// for all of them, literal arrays included. The width is also what the
/// statement was asked to do work for.
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

/// `GENERIC_PLAN` asks for a plan with the parameters left UNBOUND, which the
/// extended protocol cannot express: sea-orm prepares every statement, so the
/// bind that follows supplies none of the `$n` the EXPLAIN declares and
/// Postgres refuses the message. The simple protocol sends the text as it
/// stands, and is the only way to ask for this plan at all.
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

/// `reltuples` per relation, which is what decides both the sequential-scan rule
/// and whether a query is measurable at all. `run_all` ANALYZEs first, so the
/// estimate is fresh.
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
