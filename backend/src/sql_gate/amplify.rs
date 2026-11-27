/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::disallowed_methods,
    reason = "the amplifier writes test data and is not a statement the gate measures"
)]

//! A budget measured on the VM's 951 derivations is meaningless because a sequential scan is the
//! planner's correct choice there. Clones are copying real rows to keep the graph's own
//! distribution. Every key is derived from the original and the copy index. A copy of a referencing
//! row is then pointing at the referenced row's copy, and the database is growing as one consistent
//! graph.

use std::collections::HashMap;

use anyhow::{Context, Result};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, Value};

pub enum Rewrite {
    Remap,
    Rehash,
    References,
    Mark,
}

enum Scale {
    To(u64),
    With(&'static str),
}

struct Target {
    table: &'static str,
    scale: Scale,
    rewrite: &'static [(&'static str, Rewrite)],
}

/// Tables are listed after the tables they point at. `build_job` is appearing twice on purpose. The
/// first pass is giving the live evaluation a full-size job set. The second pass is making the
/// table many evaluations wide, keeping a lookup by evaluation as selective as in production.
const TARGETS: &[Target] = &[
    Target {
        table: "project",
        scale: Scale::To(20),
        rewrite: &[("id", Rewrite::Remap), ("name", Rewrite::Mark)],
    },
    Target {
        table: "task",
        scale: Scale::With("project"),
        rewrite: &[("id", Rewrite::Remap), ("project", Rewrite::Remap)],
    },
    Target {
        table: "evaluation",
        scale: Scale::With("project"),
        rewrite: &[
            ("id", Rewrite::Remap),
            ("task", Rewrite::Remap),
            ("previous", Rewrite::Remap),
            ("next", Rewrite::Remap),
        ],
    },
    Target {
        table: "entry_point",
        scale: Scale::With("project"),
        rewrite: &[
            ("id", Rewrite::Remap),
            ("task", Rewrite::Remap),
            ("evaluation", Rewrite::Remap),
        ],
    },
    Target {
        table: "derivation",
        scale: Scale::To(100_000),
        rewrite: &[
            ("id", Rewrite::Remap),
            ("hash", Rewrite::Rehash),
            ("name", Rewrite::Mark),
        ],
    },
    Target {
        table: "derivation_build",
        scale: Scale::With("derivation"),
        rewrite: &[("id", Rewrite::Remap), ("derivation", Rewrite::Remap)],
    },
    Target {
        table: "cached_path",
        scale: Scale::With("derivation"),
        rewrite: &[
            ("id", Rewrite::Remap),
            ("hash", Rewrite::Rehash),
            ("package", Rewrite::Mark),
            ("references", Rewrite::References),
        ],
    },
    Target {
        table: "cached_path_signature",
        scale: Scale::With("derivation"),
        rewrite: &[("id", Rewrite::Remap), ("cached_path", Rewrite::Remap)],
    },
    Target {
        table: "derivation_output",
        scale: Scale::With("derivation"),
        rewrite: &[
            ("id", Rewrite::Remap),
            ("derivation", Rewrite::Remap),
            ("hash", Rewrite::Rehash),
            ("cached_path", Rewrite::Remap),
            ("references_list", Rewrite::References),
        ],
    },
    Target {
        table: "derivation_dependency",
        scale: Scale::With("derivation"),
        rewrite: &[
            ("derivation", Rewrite::Remap),
            ("dependency", Rewrite::Remap),
        ],
    },
    Target {
        table: "derivation_input_source",
        scale: Scale::With("derivation"),
        rewrite: &[("derivation", Rewrite::Remap), ("hash", Rewrite::Rehash)],
    },
    Target {
        table: "debug_info",
        scale: Scale::With("derivation"),
        rewrite: &[("id", Rewrite::Remap), ("cached_path", Rewrite::Remap)],
    },
    Target {
        table: "build_job",
        scale: Scale::With("derivation"),
        rewrite: &[
            ("id", Rewrite::Remap),
            ("derivation", Rewrite::Remap),
            ("derivation_build", Rewrite::Remap),
        ],
    },
    Target {
        table: "dispatched_job",
        scale: Scale::With("derivation"),
        rewrite: &[("id", Rewrite::Remap)],
    },
    Target {
        table: "build_attempt",
        scale: Scale::With("derivation"),
        rewrite: &[
            ("id", Rewrite::Remap),
            ("derivation_build", Rewrite::Remap),
            ("dispatched_job", Rewrite::Remap),
            ("build_job", Rewrite::Remap),
        ],
    },
    Target {
        table: "build_job",
        scale: Scale::With("project"),
        rewrite: &[("id", Rewrite::Remap), ("evaluation", Rewrite::Remap)],
    },
];

pub async fn run(db: &DatabaseConnection, scale: u32) -> Result<()> {
    let mut counts: HashMap<&str, i32> = HashMap::new();

    for target in TARGETS {
        let live = count(db, target.table).await?;
        if live == 0 {
            println!("amplify: {} is empty, skipped", target.table);
            continue;
        }

        let copies = match target.scale {
            Scale::To(rows) => {
                let copies = i32::try_from((rows / u64::from(scale.max(1)) / live).max(1))
                    .with_context(|| {
                        format!(
                            "Copy count for {} does not fit in a 32-bit integer",
                            target.table
                        )
                    })?;
                counts.insert(target.table, copies);
                copies
            }

            Scale::With(table) => *counts
                .get(table)
                .with_context(|| format!("{} follows {table}, which set no count", target.table))?,
        };

        let columns = columns(db, target.table).await?;
        let sql = clone_sql(target.table, &columns, target.rewrite);

        execute(db, &sql, copies)
            .await
            .with_context(|| format!("amplify {}", target.table))?;

        analyze(db, target.table).await?;

        println!(
            "amplify: {} {live} rows x {copies} copies -> {} rows",
            target.table,
            count(db, target.table).await?,
        );
    }

    Ok(())
}

pub fn quoted(column: &str) -> String {
    format!("\"{}\"", column.replace('"', "\"\""))
}

pub fn clone_sql(table: &str, columns: &[String], rewrite: &[(&str, Rewrite)]) -> String {
    let exprs: Vec<String> = columns
        .iter()
        .map(|column| {
            let q = quoted(column);
            match rewrite
                .iter()
                .find(|(name, _)| name == column)
                .map(|(_, r)| r)
            {
                Some(Rewrite::Remap) => format!("md5(t.{q}::text || g.i::text)::uuid"),
                Some(Rewrite::Rehash) => format!("substr(md5(t.{q} || g.i::text), 1, 32)"),
                Some(Rewrite::References) => format!(
                    "(SELECT string_agg(regexp_replace(r.tok, '[0-9a-z]{{32}}', \
                     substr(md5(substring(r.tok FROM '[0-9a-z]{{32}}') || g.i::text), 1, 32)), \
                     ' ' ORDER BY r.n) \
                     FROM unnest(string_to_array(t.{q}, ' ')) WITH ORDINALITY AS r(tok, n))"
                ),
                Some(Rewrite::Mark) => format!("'amp' || g.i::text || '-' || t.{q}"),
                None => format!("t.{q}"),
            }
        })
        .collect();

    format!(
        "INSERT INTO {table} ({}) SELECT {} FROM {table} t, generate_series(1, $1) g(i) \
         ON CONFLICT DO NOTHING",
        columns
            .iter()
            .map(|c| quoted(c))
            .collect::<Vec<_>>()
            .join(", "),
        exprs.join(", "),
    )
}

async fn execute(db: &DatabaseConnection, sql: &str, copies: i32) -> Result<()> {
    db.execute_raw(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        sql,
        [Value::from(copies)],
    ))
    .await
    .with_context(|| format!("statement failed:\n{sql}"))?;

    Ok(())
}

async fn analyze(db: &DatabaseConnection, table: &str) -> Result<()> {
    let sql = format!("ANALYZE {table}");
    db.execute_unprepared(&sql)
        .await
        .with_context(|| format!("statement failed:\n{sql}"))?;

    Ok(())
}

pub async fn columns(db: &DatabaseConnection, table: &str) -> Result<Vec<String>> {
    const SQL: &str = "SELECT column_name AS name FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = $1 \
         ORDER BY ordinal_position";

    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            SQL,
            [Value::from(table)],
        ))
        .await
        .with_context(|| format!("statement failed:\n{SQL}\n-- $1 = {table}"))?;

    rows.into_iter()
        .map(|row| row.try_get::<String>("", "name").map_err(Into::into))
        .collect()
}

async fn count(db: &DatabaseConnection, table: &str) -> Result<u64> {
    let sql = format!("SELECT count(*) AS n FROM {table}");
    let row = db
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            sql.clone(),
        ))
        .await
        .with_context(|| format!("statement failed:\n{sql}"))?
        .context("count returned no row")?;

    Ok(row.try_get::<i64>("", "n")? as u64)
}

#[cfg(test)]
mod tests {
    use super::{Scale, TARGETS, clone_sql, columns, execute};
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::shape::shape;
    use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};

    const DEPENDENT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-one.drv";
    const DEPENDENCY: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-two.drv";

    async fn scalar(db: &DatabaseConnection, sql: &str) -> i64 {
        db.query_one_raw(Statement::from_string(DatabaseBackend::Postgres, sql))
            .await
            .expect("query")
            .expect("a row")
            .try_get_by_index(0)
            .expect("a bigint")
    }

    async fn clone_table(db: &TestDatabase, table: &str, copies: i32) {
        let target = TARGETS
            .iter()
            .find(|target| target.table == table)
            .expect("a target");
        let connection = db.connection();
        let columns = columns(&connection, table).await.expect("columns");
        let sql = clone_sql(table, &columns, target.rewrite);
        execute(&connection, &sql, copies).await.expect("clone");
    }

    #[tokio::test]
    async fn test_sql_gate_clone_statement_copies_rows_with_remapped_keys_rehashed_hashes_marked_names_and_remapped_references()
     {
        let (hash_a, hash_b) = ("a".repeat(32), "b".repeat(32));
        let db = shape()
            .derivation(DEPENDENT)
            .depends_on(DEPENDENCY)
            .derivation(DEPENDENCY)
            .cached_path(&hash_a, "one")
            .references(&[(&hash_b, "two")])
            .cached_path(&hash_b, "two")
            .into_database()
            .await;
        let connection = db.connection();

        clone_table(&db, "derivation", 2).await;
        clone_table(&db, "derivation_dependency", 2).await;
        clone_table(&db, "cached_path", 2).await;

        assert_eq!(
            scalar(&connection, "SELECT count(*) FROM derivation").await,
            6
        );
        assert_eq!(
            scalar(&connection, "SELECT count(DISTINCT hash) FROM derivation").await,
            6
        );
        assert_eq!(
            scalar(
                &connection,
                "SELECT count(*) FROM derivation WHERE name LIKE 'amp_-%'"
            )
            .await,
            4
        );
        assert_eq!(
            scalar(
                &connection,
                "SELECT count(DISTINCT created_at) FROM derivation"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(&connection, "SELECT count(*) FROM derivation_dependency").await,
            3
        );
        assert_eq!(
            scalar(
                &connection,
                "SELECT count(*) FROM derivation_dependency e \
                 JOIN derivation a ON a.id = e.derivation \
                 JOIN derivation b ON b.id = e.dependency \
                 WHERE a.name LIKE 'amp_-one' AND b.name = replace(a.name, 'one', 'two')"
            )
            .await,
            2
        );

        assert_eq!(
            scalar(&connection, "SELECT count(*) FROM cached_path").await,
            6
        );
        assert_eq!(
            scalar(&connection, "SELECT count(DISTINCT hash) FROM cached_path").await,
            6
        );
        assert_eq!(
            scalar(
                &connection,
                "SELECT count(DISTINCT nar_size) FROM cached_path"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(
                &connection,
                "SELECT count(*) FROM cached_path a JOIN cached_path b \
                 ON a.\"references\" = b.hash || '-two' \
                 AND replace(a.package, 'one', 'two') = b.package \
                 WHERE a.package LIKE 'amp_-one'"
            )
            .await,
            2
        );
        db.finish().await;
    }

    #[test]
    fn every_followed_table_is_cloned_before_the_tables_that_follow_it() {
        let mut set: Vec<&str> = Vec::new();

        for target in TARGETS {
            match target.scale {
                Scale::To(_) => set.push(target.table),
                Scale::With(table) => {
                    assert!(set.contains(&table), "{} follows {table}", target.table)
                }
            }
        }
    }
}
