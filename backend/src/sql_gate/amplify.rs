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
//! ones, which keeps the graph's own distribution.

use anyhow::{Context, Result};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, Value};

pub enum Rewrite {
    /// Primary key: a fresh uuid per copy.
    NewUuid,
    /// Unique 32-char store hash: deterministic per copy, so an edge table can
    /// join a clone to its original without a mapping table.
    Rehash,
    /// The marker column. Its `amp-` prefix is what keeps a clone from being
    /// cloned again, and what a later phase would exclude on.
    Mark,
    /// FK to `derivation`: the row follows its derivation, so the copy points at
    /// the copy of that derivation and never at the original. A table with one
    /// of these carries no marker of its own - the derivation it hangs off is
    /// what says whether the row is already a clone.
    ClonedDerivation,
    /// FK to `derivation_build`: the anchor of that copied derivation. Joined
    /// rather than computed, so a copy with no anchor is dropped instead of
    /// pointing at a row that does not exist.
    ClonedAnchor,
}

struct Target {
    table: &'static str,
    rows: u64,
    rewrite: &'static [(&'static str, Rewrite)],
}

const TARGETS: &[Target] = &[
    Target {
        table: "derivation",
        rows: 500_000,
        rewrite: &[
            ("id", Rewrite::NewUuid),
            ("hash", Rewrite::Rehash),
            ("name", Rewrite::Mark),
        ],
    },
    Target {
        table: "cached_path",
        rows: 300_000,
        rewrite: &[
            ("id", Rewrite::NewUuid),
            ("hash", Rewrite::Rehash),
            ("package", Rewrite::Mark),
        ],
    },
    Target {
        table: "derivation_build",
        rows: 500_000,
        rewrite: &[
            ("id", Rewrite::NewUuid),
            ("derivation", Rewrite::ClonedDerivation),
        ],
    },
    Target {
        table: "build_job",
        rows: 200_000,
        rewrite: &[
            ("id", Rewrite::NewUuid),
            ("derivation", Rewrite::ClonedDerivation),
            ("derivation_build", Rewrite::ClonedAnchor),
        ],
    },
    Target {
        table: "project",
        rows: 10_000,
        rewrite: &[("id", Rewrite::NewUuid), ("name", Rewrite::Mark)],
    },
];

const EDGE_DEPENDENCY: &str = "\
INSERT INTO derivation_dependency (derivation, dependency) \
SELECT ca.id, cb.id \
FROM derivation_dependency e \
JOIN derivation oa ON oa.id = e.derivation \
JOIN derivation ob ON ob.id = e.dependency \
CROSS JOIN generate_series(1, $1) g(i) \
JOIN derivation ca ON ca.hash = substr(md5(oa.hash || g.i::text), 1, 32) \
JOIN derivation cb ON cb.hash = substr(md5(ob.hash || g.i::text), 1, 32) \
WHERE oa.name NOT LIKE 'amp-%' AND ob.name NOT LIKE 'amp-%' \
ON CONFLICT DO NOTHING";

const EDGE_REFERENCE: &str = "\
INSERT INTO cached_path_reference (referrer, reference, reference_hash, position) \
SELECT ca.hash, ca.hash || '-' || r.position::text, cb.hash, r.position \
FROM cached_path_reference r \
JOIN cached_path oa ON oa.hash = r.referrer \
JOIN cached_path ob ON ob.hash = r.reference_hash \
CROSS JOIN generate_series(1, $1) g(i) \
JOIN cached_path ca ON ca.hash = substr(md5(oa.hash || g.i::text), 1, 32) \
JOIN cached_path cb ON cb.hash = substr(md5(ob.hash || g.i::text), 1, 32) \
WHERE oa.package NOT LIKE 'amp-%' AND ob.package NOT LIKE 'amp-%' \
ON CONFLICT DO NOTHING";

pub async fn run(db: &DatabaseConnection, scale: u32) -> Result<()> {
    for target in TARGETS {
        let live = count(db, target.table).await?;
        if live == 0 {
            println!("amplify: {} is empty, skipped", target.table);
            continue;
        }

        let copies = (target.rows / u64::from(scale.max(1)) / live).max(1);
        let columns = columns(db, target.table).await?;
        let sql = clone_sql(target.table, &columns, target.rewrite);

        execute(db, &sql, copies as i32)
            .await
            .with_context(|| format!("amplifying {}", target.table))?;

        analyze(db, target.table).await?;

        println!(
            "amplify: {} {live} rows x {copies} copies -> {} rows",
            target.table,
            count(db, target.table).await?,
        );
    }

    for (table, sql) in [
        ("derivation_dependency", EDGE_DEPENDENCY),
        ("cached_path_reference", EDGE_REFERENCE),
    ] {
        let copies = edge_copies(db, table, scale).await?;
        execute(db, sql, copies)
            .await
            .with_context(|| format!("amplifying {table}"))?;

        analyze(db, table).await?;
        println!("amplify: {table} -> {} rows", count(db, table).await?);
    }

    Ok(())
}

/// One clone pass over `table`: every column is copied verbatim unless the
/// rewrite list gives it a new value. The column list comes from the database,
/// so a schema change cannot leave a stale one behind.
pub fn clone_sql(table: &str, columns: &[String], rewrite: &[(&str, Rewrite)]) -> String {
    let exprs: Vec<String> = columns
        .iter()
        .map(|column| {
            match rewrite
                .iter()
                .find(|(name, _)| name == column)
                .map(|(_, r)| r)
            {
                Some(Rewrite::NewUuid) => "uuidv7()".to_string(),
                Some(Rewrite::Rehash) => format!("substr(md5(t.{column} || g.i::text), 1, 32)"),
                Some(Rewrite::Mark) => format!("'amp-' || t.{column}"),
                Some(Rewrite::ClonedDerivation) => "c.id".to_string(),
                Some(Rewrite::ClonedAnchor) => "cb.id".to_string(),
                None => format!("t.{column}"),
            }
        })
        .collect();

    let derivation = rewrite
        .iter()
        .find(|(_, r)| matches!(r, Rewrite::ClonedDerivation))
        .map(|(column, _)| *column);

    let (from, guard) = match derivation {
        Some(column) => (
            format!(
                "FROM {table} t CROSS JOIN generate_series(1, $1) g(i) \
                 JOIN derivation o ON o.id = t.{column} \
                 JOIN derivation c ON c.hash = substr(md5(o.hash || g.i::text), 1, 32){anchor}",
                anchor = if rewrite
                    .iter()
                    .any(|(_, r)| matches!(r, Rewrite::ClonedAnchor))
                {
                    " JOIN derivation_build cb ON cb.derivation = c.id"
                } else {
                    ""
                },
            ),
            " WHERE o.name NOT LIKE 'amp-%'".to_string(),
        ),
        None => (
            format!("FROM {table} t, generate_series(1, $1) g(i)"),
            rewrite
                .iter()
                .find(|(_, r)| matches!(r, Rewrite::Mark))
                .map(|(column, _)| format!(" WHERE t.{column} NOT LIKE 'amp-%'"))
                .unwrap_or_default(),
        ),
    };

    format!(
        "INSERT INTO {table} ({}) SELECT {} {from}{guard} ON CONFLICT DO NOTHING",
        columns.join(", "),
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

/// Edges follow their endpoints: cloning them more often than the derivations
/// they join would only re-insert the same pairs.
async fn edge_copies(db: &DatabaseConnection, table: &str, scale: u32) -> Result<i32> {
    let endpoints = match table {
        "cached_path_reference" => count(db, "cached_path").await?,
        _ => count(db, "derivation").await?,
    };

    let target = match table {
        "cached_path_reference" => 300_000,
        _ => 500_000,
    };

    Ok((target / u64::from(scale.max(1)) / endpoints.max(1)).max(1) as i32)
}

#[cfg(test)]
mod tests {
    use super::{Rewrite, clone_sql};

    #[test]
    fn a_clone_rewrites_the_key_and_copies_the_rest() {
        let columns = ["id".to_string(), "hash".to_string(), "name".to_string()];
        let sql = clone_sql(
            "derivation",
            &columns,
            &[
                ("id", Rewrite::NewUuid),
                ("hash", Rewrite::Rehash),
                ("name", Rewrite::Mark),
            ],
        );

        assert!(
            sql.starts_with("INSERT INTO derivation (id, hash, name)"),
            "{sql}"
        );
        assert!(sql.contains("uuidv7()"), "{sql}");
        assert!(
            sql.contains("substr(md5(t.hash || g.i::text), 1, 32)"),
            "{sql}"
        );
        assert!(sql.contains("'amp-' || t.name"), "{sql}");
        assert!(sql.contains("generate_series(1, $1) g(i)"), "{sql}");
        assert!(sql.ends_with("ON CONFLICT DO NOTHING"), "{sql}");
    }

    #[test]
    fn an_unrewritten_column_is_copied_verbatim() {
        let columns = ["id".to_string(), "created_at".to_string()];
        let sql = clone_sql("derivation", &columns, &[("id", Rewrite::NewUuid)]);
        assert!(sql.contains("t.created_at"), "{sql}");
    }

    #[test]
    fn a_row_that_hangs_off_a_derivation_follows_its_copy() {
        let columns = [
            "id".to_string(),
            "derivation".to_string(),
            "derivation_build".to_string(),
        ];
        let sql = clone_sql(
            "build_job",
            &columns,
            &[
                ("id", Rewrite::NewUuid),
                ("derivation", Rewrite::ClonedDerivation),
                ("derivation_build", Rewrite::ClonedAnchor),
            ],
        );

        assert!(sql.contains("SELECT uuidv7(), c.id, cb.id"), "{sql}");
        assert!(
            sql.contains("JOIN derivation o ON o.id = t.derivation"),
            "{sql}"
        );
        assert!(
            sql.contains("JOIN derivation c ON c.hash = substr(md5(o.hash || g.i::text), 1, 32)"),
            "{sql}"
        );
        assert!(
            sql.contains("JOIN derivation_build cb ON cb.derivation = c.id"),
            "{sql}"
        );
        assert!(sql.contains("WHERE o.name NOT LIKE 'amp-%'"), "{sql}");
    }

    #[test]
    fn the_clone_never_reads_its_own_output() {
        let columns = ["id".to_string(), "name".to_string()];
        let sql = clone_sql("derivation", &columns, &[("name", Rewrite::Mark)]);
        assert!(sql.contains("WHERE t.name NOT LIKE 'amp-%'"), "{sql}");
    }
}
