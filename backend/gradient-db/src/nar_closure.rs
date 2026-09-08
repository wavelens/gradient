/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `cached_path.missing_references`: how many of a path's references are not
//! whole. Seeded when the NAR is committed, moved by a frontier ripple over
//! `cached_path_reference` when a path becomes or stops being whole, never
//! re-derived by a sweep. `whole_predicate` is the one definition every gate
//! reads.

use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, Statement};

/// The row `{alias}` is stored and every reference resolves to a whole row.
pub fn whole_predicate(alias: &str) -> String {
    format!("({alias}.file_hash IS NOT NULL AND {alias}.missing_references = 0)")
}

const SEED: &str = r#"
    UPDATE cached_path cp SET missing_references = (
        SELECT count(*) FROM cached_path_reference r
        LEFT JOIN cached_path dep ON dep.hash = r.reference_hash
        WHERE r.referrer = cp.hash
          AND r.reference_hash <> cp.hash
          AND NOT (dep.file_hash IS NOT NULL AND dep.missing_references = 0))
    WHERE cp.hash = $1
    RETURNING cp.file_hash IS NOT NULL AND cp.missing_references = 0 AS whole
"#;

const FORWARD: &str = r#"
    UPDATE cached_path cp
    SET missing_references = cp.missing_references - c.n
    FROM (SELECT r.referrer, count(*) AS n FROM cached_path_reference r
          WHERE r.reference_hash = ANY($1) AND r.referrer <> r.reference_hash
          GROUP BY r.referrer) c
    WHERE cp.hash = c.referrer
    RETURNING cp.hash, cp.file_hash IS NOT NULL AND cp.missing_references = 0 AS whole
"#;

const REVERSE: &str = r#"
    UPDATE cached_path cp
    SET missing_references = cp.missing_references + c.n
    FROM (SELECT r.referrer, count(*) AS n FROM cached_path_reference r
          WHERE r.reference_hash = ANY($1) AND r.referrer <> r.reference_hash
          GROUP BY r.referrer) c
    WHERE cp.hash = c.referrer
    RETURNING cp.hash, cp.file_hash IS NOT NULL AND cp.missing_references = c.n AS was_whole
"#;

const DELETE: &str = r#"
    DELETE FROM cached_path cp WHERE cp.hash = ANY($1)
    RETURNING cp.hash, cp.file_hash IS NOT NULL AND cp.missing_references = 0 AS was_whole
"#;

/// Compute the counter of a just-committed row from its references. Returns
/// whether the row is whole, which is what the caller ripples forward.
pub async fn seed_references<C: ConnectionTrait>(db: &C, hash: &str) -> Result<bool, DbErr> {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            SEED,
            [hash.into()],
        ))
        .await?;

    Ok(row
        .and_then(|r| r.try_get::<bool>("", "whole").ok())
        .unwrap_or(false))
}

/// One level per statement: decrement the referrers of the frontier and
/// continue from those that reached zero. Returns every hash that became
/// whole, the seeds included.
pub async fn ripple_whole<C: ConnectionTrait>(
    db: &C,
    became_whole: Vec<String>,
) -> Result<Vec<String>, DbErr> {
    ripple(db, FORWARD, "whole", became_whole).await
}

/// The reverse: increment the referrers of the frontier and continue from
/// those that were whole before. Returns every hash that stopped being whole,
/// the seeds included.
pub async fn ripple_unwhole<C: ConnectionTrait>(
    db: &C,
    stopped_being_whole: Vec<String>,
) -> Result<Vec<String>, DbErr> {
    ripple(db, REVERSE, "was_whole", stopped_being_whole).await
}

async fn ripple<C: ConnectionTrait>(
    db: &C,
    statement: &str,
    flag: &str,
    seeds: Vec<String>,
) -> Result<Vec<String>, DbErr> {
    let mut all = seeds.clone();
    let mut frontier = seeds;
    while !frontier.is_empty() {
        let rows = db
            .query_all_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                statement,
                [frontier.into()],
            ))
            .await?;

        frontier = rows
            .iter()
            .filter(|r| r.try_get::<bool>("", flag).unwrap_or(false))
            .filter_map(|r| r.try_get::<String>("", "hash").ok())
            .collect();
        all.extend(frontier.iter().cloned());
    }

    Ok(all)
}

/// What `retire_paths` removed and what stopped being whole because of it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Retired {
    pub deleted: Vec<String>,
    pub unwhole: Vec<String>,
}

/// Delete `hashes` from the index and move every counter and flag that trusted
/// them, in the caller's transaction: the reverse ripple from the rows that
/// were whole, `is_cached` off the outputs, and the anchor flags the rows
/// backed (`drv_closure_cached` on the owners of a deleted `.drv`,
/// `closure_complete` on the producers of every hash that stopped being whole).
pub async fn retire_paths<C: ConnectionTrait>(db: &C, hashes: &[String]) -> Result<Retired, DbErr> {
    if hashes.is_empty() {
        return Ok(Retired::default());
    }

    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            DELETE,
            [hashes.to_vec().into()],
        ))
        .await?;

    let deleted: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String>("", "hash").ok())
        .collect();
    let were_whole: Vec<String> = rows
        .iter()
        .filter(|r| r.try_get::<bool>("", "was_whole").unwrap_or(false))
        .filter_map(|r| r.try_get::<String>("", "hash").ok())
        .collect();

    let unwhole = ripple_unwhole(db, were_whole).await?;

    db.execute_raw(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE derivation_output SET is_cached = false WHERE is_cached AND hash = ANY($1)",
        [deleted.clone().into()],
    ))
    .await?;

    db.execute_raw(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"
        UPDATE derivation_build db SET drv_closure_cached = false
        FROM derivation d
        WHERE d.id = db.derivation AND db.drv_closure_cached AND d.hash = ANY($1)
        "#,
        [unwhole.clone().into()],
    ))
    .await?;

    db.execute_raw(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"
        UPDATE derivation_build db SET closure_complete = false
        WHERE db.closure_complete
          AND db.derivation IN (SELECT o.derivation FROM derivation_output o WHERE o.hash = ANY($1))
        "#,
        [unwhole.clone().into()],
    ))
    .await?;

    Ok(Retired { deleted, unwhole })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn row(hash: &str, flag: &str, value: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("hash".to_owned(), Value::from(hash.to_owned())),
            (flag.to_owned(), Value::from(value)),
        ])
    }

    fn statements(db: sea_orm::DatabaseConnection) -> Vec<String> {
        db.into_transaction_log()
            .iter()
            .map(|t| format!("{t:?}"))
            .collect()
    }

    /// A reference counts as missing when its row is absent, unbacked or not
    /// whole; a self-reference never counts, or a self-referential path could
    /// never be whole.
    #[test]
    fn the_seed_counts_absent_unbacked_and_unwhole_references_but_not_self() {
        let sql = norm(SEED);
        assert!(sql.contains("LEFT JOIN cached_path dep ON dep.hash = r.reference_hash"));
        assert!(sql.contains("r.reference_hash <> cp.hash"));
        assert!(sql.contains("NOT (dep.file_hash IS NOT NULL AND dep.missing_references = 0)"));
        assert!(
            sql.contains(
                "RETURNING cp.file_hash IS NOT NULL AND cp.missing_references = 0 AS whole"
            )
        );
        assert_eq!(
            norm(&whole_predicate("cp")),
            "(cp.file_hash IS NOT NULL AND cp.missing_references = 0)"
        );
    }

    /// The forward ripple decrements every referrer of the frontier once per
    /// reference and carries on only from referrers that reached zero.
    #[tokio::test]
    async fn the_forward_ripple_continues_only_from_rows_that_became_whole() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row("r1", "whole", true), row("r2", "whole", false)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let whole = ripple_whole(&db, vec!["seed".to_owned()]).await.unwrap();

        assert_eq!(whole, vec!["seed".to_owned(), "r1".to_owned()]);
        let log = statements(db);
        assert_eq!(log.len(), 2, "one statement per level: {log:?}");
        assert!(log[0].contains("missing_references - c.n") && log[0].contains("\"seed\""));
        assert!(
            log[1].contains("\"r1\"") && !log[1].contains("\"r2\""),
            "{log:?}"
        );
    }

    /// The reverse ripple increments and carries on only from referrers that
    /// were whole before the increment.
    #[tokio::test]
    async fn the_reverse_ripple_continues_only_from_rows_that_were_whole() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                row("r1", "was_whole", true),
                row("r2", "was_whole", false),
            ]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let unwhole = ripple_unwhole(&db, vec!["gone".to_owned()]).await.unwrap();

        assert_eq!(unwhole, vec!["gone".to_owned(), "r1".to_owned()]);
        let log = statements(db);
        assert!(log[0].contains("missing_references + c.n") && log[0].contains("\"gone\""));
        assert!(
            log[1].contains("\"r1\"") && !log[1].contains("\"r2\""),
            "{log:?}"
        );
    }

    /// Retiring seeds the reverse ripple only from rows that were whole: a
    /// referrer of a row that was already incomplete counted it as missing
    /// already, so it must not be incremented twice.
    #[tokio::test]
    async fn retire_ripples_only_from_rows_that_were_whole() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                row("a", "was_whole", true),
                row("b", "was_whole", false),
            ]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                };
                3
            ])
            .into_connection();

        let retired = retire_paths(&db, &["a".to_owned(), "b".to_owned()])
            .await
            .unwrap();

        assert_eq!(retired.deleted, vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(retired.unwhole, vec!["a".to_owned()]);
        let log = statements(db);
        assert!(log[0].contains("DELETE FROM cached_path") && log[0].contains("RETURNING"));
        assert!(
            log[1].contains("missing_references + c.n")
                && log[1].contains("\"a\"")
                && !log[1].contains("\"b\"")
        );
    }

    /// Nothing to retire is a no-op: no statement at all.
    #[tokio::test]
    async fn retiring_nothing_issues_no_statement() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let retired = retire_paths(&db, &[]).await.unwrap();
        assert!(retired.deleted.is_empty() && retired.unwhole.is_empty());
        assert!(statements(db).is_empty());
    }
}
