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
                let copies = (rows / u64::from(scale.max(1)) / live).max(1) as i32;
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
            .with_context(|| format!("amplifying {}", target.table))?;

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
    use super::{Rewrite, Scale, TARGETS, clone_sql};

    #[test]
    fn a_clone_rewrites_the_key_and_copies_the_rest() {
        let columns = ["id".to_string(), "hash".to_string(), "name".to_string()];
        let sql = clone_sql(
            "derivation",
            &columns,
            &[
                ("id", Rewrite::Remap),
                ("hash", Rewrite::Rehash),
                ("name", Rewrite::Mark),
            ],
        );

        assert!(
            sql.starts_with("INSERT INTO derivation (\"id\", \"hash\", \"name\")"),
            "{sql}"
        );
        assert!(
            sql.contains("md5(t.\"id\"::text || g.i::text)::uuid"),
            "{sql}"
        );
        assert!(
            sql.contains("substr(md5(t.\"hash\" || g.i::text), 1, 32)"),
            "{sql}"
        );
        assert!(
            sql.contains("'amp' || g.i::text || '-' || t.\"name\""),
            "{sql}"
        );
        assert!(sql.contains("generate_series(1, $1) g(i)"), "{sql}");
        assert!(sql.ends_with("ON CONFLICT DO NOTHING"), "{sql}");
    }

    #[test]
    fn a_reserved_word_is_a_usable_column_name() {
        let sql = clone_sql("cached_path", &["references".to_string()], &[]);
        assert!(
            sql.starts_with("INSERT INTO cached_path (\"references\")")
                && sql.contains("t.\"references\""),
            "{sql}"
        );
    }

    #[test]
    fn an_unrewritten_column_is_copied_verbatim() {
        let columns = ["id".to_string(), "created_at".to_string()];
        let sql = clone_sql("derivation", &columns, &[("id", Rewrite::Remap)]);
        assert!(sql.contains("t.\"created_at\""), "{sql}");
    }

    #[test]
    fn a_foreign_key_lands_on_the_copy_of_the_row_it_named() {
        let key = clone_sql("derivation", &["id".to_string()], &[("id", Rewrite::Remap)]);
        let fk = clone_sql(
            "derivation_build",
            &["derivation".to_string()],
            &[("derivation", Rewrite::Remap)],
        );

        assert!(
            key.contains("md5(t.\"id\"::text || g.i::text)::uuid"),
            "{key}"
        );
        assert!(
            fk.contains("md5(t.\"derivation\"::text || g.i::text)::uuid"),
            "{fk}"
        );
    }

    #[test]
    fn a_reference_lands_on_the_copy_of_the_path_it_named() {
        let hash = clone_sql(
            "cached_path",
            &["hash".to_string()],
            &[("hash", Rewrite::Rehash)],
        );
        let references = clone_sql(
            "cached_path",
            &["references".to_string()],
            &[("references", Rewrite::References)],
        );

        assert!(
            hash.contains("substr(md5(t.\"hash\" || g.i::text), 1, 32)"),
            "{hash}"
        );
        assert!(
            references
                .contains("substr(md5(substring(r.tok FROM '[0-9a-z]{32}') || g.i::text), 1, 32)")
                && references.contains("unnest(string_to_array(t.\"references\", ' '))"),
            "{references}"
        );
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
