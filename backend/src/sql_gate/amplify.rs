/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::disallowed_methods,
    reason = "the amplifier writes test data and is not a statement the gate measures"
)]

//! Grows the e2e VM's real rows to production shape. A sequential scan of 951
//! derivations is the planner's correct choice, so a budget measured at that
//! size means nothing. Clones copy real rows rather than generating uniform
//! ones, which keeps the graph's own distribution, and every key is derived
//! from the original and the copy index: a copy of a row that referenced
//! another therefore references that row's copy, and the whole database grows
//! as one consistent graph rather than a pile of orphans.

use std::collections::HashMap;

use anyhow::{Context, Result};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, Value};

pub enum Rewrite {
    /// A uuid key or foreign key. The copy's value is a function of the
    /// original and the copy index, so a reference finds the copy of the row it
    /// pointed at without a mapping table, and a NULL stays NULL.
    Remap,
    /// A 32-char store hash, derived the same way.
    Rehash,
    /// A column a unique index covers: the copy index keeps it unique and the
    /// `amp` prefix says the row is synthetic.
    Mark,
    /// A column spelled out rather than copied, for a value derived from one of
    /// the rewritten ones.
    Expr(&'static str),
}

enum Scale {
    /// Copy until the table holds about this many rows.
    To(u64),
    /// Take another table's copy count, so a copy never points at one that was
    /// never made.
    With(&'static str),
}

struct Target {
    table: &'static str,
    scale: Scale,
    rewrite: &'static [(&'static str, Rewrite)],
}

/// Ordered by foreign key: a table is cloned only once the tables it points at
/// have been. `build_job` appears twice on purpose - once per cloned derivation
/// so the live evaluation carries a full-size job set, then once per cloned
/// evaluation so the table is many evaluations wide, which is what makes a
/// lookup by evaluation selective the way it is in production.
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
            (
                "references",
                Rewrite::Expr("substr(md5(t.hash || g.i::text), 1, 32) || '-amp'"),
            ),
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

/// A column name as an identifier. The list comes from `information_schema` in
/// the database's own case, so quoting is always safe and is the only thing that
/// lets a reserved word be a column: `cached_path.references` is one.
fn quoted(column: &str) -> String {
    format!("\"{}\"", column.replace('"', "\"\""))
}

/// One clone pass over `table`: every column is copied verbatim unless the
/// rewrite list gives it a new value. The column list comes from the database,
/// so a schema change cannot leave a stale one behind, and the pass needs no
/// guard against reading its own output because an `INSERT ... SELECT` never
/// sees the rows it is writing.
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
                Some(Rewrite::Mark) => format!("'amp' || g.i::text || '-' || t.{q}"),
                Some(Rewrite::Expr(sql)) => (*sql).to_string(),
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

/// Every amplification statement runs through here, so a failure carries the
/// statement itself: a table or column the schema no longer has is otherwise a
/// bare `relation "x" does not exist` with nothing to point at.
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

async fn columns(db: &DatabaseConnection, table: &str) -> Result<Vec<String>> {
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

    /// `cached_path.references` is a reserved word, so the gate could not amplify
    /// the table at all until every identifier was quoted.
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

    /// A foreign key and the primary key it points at are rewritten by the same
    /// function, which is what keeps a copied row pointing at copied rows.
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
