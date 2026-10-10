/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::disallowed_methods,
    reason = "the copy writes test data and is not a measured statement"
)]

//! Production's planner is meeting a new evaluation before autovacuum has analyzed it. A copy of
//! the largest evaluation, written after the last ANALYZE, is giving every evaluation draw the
//! estimate a new evaluation gets.

use anyhow::{Context, Result};
use gradient_db::sql::Param;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, Value};
use uuid::Uuid;

use crate::amplify::{columns, quoted};
use crate::sample::draw_sql;

struct Copy {
    table: &'static str,
    key: &'static str,
    rewrite: &'static [(&'static str, &'static str)],
}

const COPIES: &[Copy] = &[
    Copy {
        table: "evaluation",
        key: "id",
        rewrite: &[
            ("id", "$2::uuid"),
            ("next", "NULL"),
            ("created_at", "now()"),
            ("concurrent", "true"),
        ],
    },
    Copy {
        table: "build_job",
        key: "evaluation",
        rewrite: &[("id", "uuidv7()"), ("evaluation", "$2::uuid")],
    },
    Copy {
        table: "entry_point",
        key: "evaluation",
        rewrite: &[
            ("id", "uuidv7()"),
            ("evaluation", "$2::uuid"),
            ("dep_counts_version", "NULL"),
            ("dep_counts_computed_at", "NULL"),
        ],
    },
];

pub async fn copy_largest_evaluation(db: &DatabaseConnection) -> Result<Option<Uuid>> {
    let Some(largest) = largest_evaluation(db).await? else {
        return Ok(None);
    };

    let copy_id = Uuid::now_v7();
    for copy in COPIES {
        db.execute_unprepared(&format!(
            "ALTER TABLE {} SET (autovacuum_enabled = false)",
            copy.table
        ))
        .await?;
        let sql = copy_sql(copy, &columns(db, copy.table).await?);
        db.execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            [Value::from(largest), Value::from(copy_id)],
        ))
        .await
        .with_context(|| format!("statement failed:\n{sql}"))?;
    }

    // A plain VACUUM can mark the copied pages all-visible for the index-only scans without
    // touching the statistics.
    db.execute_unprepared(&format!(
        "VACUUM {}",
        COPIES
            .iter()
            .map(|copy| copy.table)
            .collect::<Vec<_>>()
            .join(", ")
    ))
    .await
    .context("VACUUM of the copied evaluation")?;

    println!("unanalyzed: evaluation {copy_id} copies {largest}");
    Ok(Some(copy_id))
}

async fn largest_evaluation(db: &DatabaseConnection) -> Result<Option<Uuid>> {
    let sql = draw_sql(&Param::EvaluationId).context("evaluations have a sampler")?;
    let row = db
        .query_one_raw(Statement::from_string(DatabaseBackend::Postgres, sql))
        .await?;

    Ok(match row {
        Some(row) => row.try_get::<Option<Uuid>>("", "v")?,
        None => None,
    })
}

fn copy_sql(copy: &Copy, columns: &[String]) -> String {
    let exprs: Vec<String> = columns
        .iter()
        .map(|column| {
            copy.rewrite
                .iter()
                .find(|(name, _)| name == column)
                .map_or_else(
                    || format!("t.{}", quoted(column)),
                    |(_, expr)| expr.to_string(),
                )
        })
        .collect();

    format!(
        "INSERT INTO {table} ({}) SELECT {} FROM {table} t WHERE t.{} = $1::uuid",
        columns
            .iter()
            .map(|c| quoted(c))
            .collect::<Vec<_>>()
            .join(", "),
        exprs.join(", "),
        quoted(copy.key),
        table = copy.table,
    )
}
