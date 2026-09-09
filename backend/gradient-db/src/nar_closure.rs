/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `cached_path.missing_references`: how many of a path's references are not
//! whole. Seeded when the NAR is committed and moved by a frontier ripple over
//! `cached_path_reference` when a path becomes or stops being whole; no sweep
//! re-derives it here, and #591 recomputes it only over a bounded scope.
//! `whole_predicate` is the one definition every gate reads.
//!
//! Because the counter is moved and not recomputed, every ripple must be driven
//! by a TRANSITION, never by a state: rippling from a row that did not just flip, or
//! rippling one frontier twice, moves referrers past zero, and a negative
//! counter never satisfies `= 0` again. The ripples read that transition from
//! their own `RETURNING`; the seed cannot (see [`seed_references`]), so its
//! caller holds the pre-commit endpoint.

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
    RETURNING (cp.file_hash IS NOT NULL AND cp.missing_references = 0) AS whole
"#;

const FORWARD: &str = r#"
    UPDATE cached_path cp
    SET missing_references = cp.missing_references - c.n
    FROM (SELECT r.referrer, count(*) AS n FROM cached_path_reference r
          WHERE r.reference_hash = ANY($1) AND r.referrer <> r.reference_hash
          GROUP BY r.referrer) c
    WHERE cp.hash = c.referrer
    RETURNING cp.hash, (cp.file_hash IS NOT NULL AND cp.missing_references = 0) AS whole
"#;

const REVERSE: &str = r#"
    UPDATE cached_path cp
    SET missing_references = cp.missing_references + c.n
    FROM (SELECT r.referrer, count(*) AS n FROM cached_path_reference r
          WHERE r.reference_hash = ANY($1) AND r.referrer <> r.reference_hash
          GROUP BY r.referrer) c
    WHERE cp.hash = c.referrer
    RETURNING cp.hash, (cp.file_hash IS NOT NULL AND cp.missing_references = c.n) AS was_whole
"#;

fn delete_statement(guard: Option<&str>) -> String {
    let guard = guard.map(|g| format!(" AND ({g})")).unwrap_or_default();
    format!(
        "DELETE FROM cached_path cp WHERE cp.hash = ANY($1){guard} \
         RETURNING cp.hash, {whole} AS was_whole",
        whole = whole_predicate("cp"),
    )
}

/// Compute the counter of a just-committed row from its references and report
/// whether the row IS whole afterwards (`false` when no such row exists).
///
/// This is a STATE, and the ripples need a TRANSITION, so the caller owns the
/// other endpoint: the row's wholeness BEFORE the commit
/// ([`gradient_entity::cached_path::Model::is_whole`] on the row the commit read
/// before writing it), and it ripples only when the two differ. The statement
/// cannot report that endpoint itself - by the time it runs, the commit has
/// already stored the NAR, so a `FROM cached_path old` self-join sees a row that
/// is backed and, on a fresh insert, counts zero: whole. Reading the flip from
/// that would report `false` for exactly the commit that must ripple, and every
/// referrer of a re-pushed path would count it as missing forever.
pub async fn seed_references<C: ConnectionTrait>(db: &C, hash: &str) -> Result<bool, DbErr> {
    let Some(row) = db
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            SEED,
            [hash.into()],
        ))
        .await?
    else {
        return Ok(false);
    };

    row.try_get::<bool>("", "whole")
}

/// One level per statement: decrement the referrers of the frontier and
/// continue from those that reached zero. Returns every hash that became
/// whole, the seeds included.
///
/// Every seed must be a row that JUST became whole (see [`seed_references`]),
/// and no seed may reference another seed: the statement decrements a referrer
/// once per edge into the batch, so a seed that was already whole, or one whose
/// referrer is itself a seed, drives that referrer's counter below zero.
pub async fn ripple_whole<C: ConnectionTrait>(
    db: &C,
    became_whole: Vec<String>,
) -> Result<Vec<String>, DbErr> {
    ripple(db, FORWARD, "whole", became_whole).await
}

/// The reverse: increment the referrers of the frontier and continue from
/// those that were whole before. Returns every hash that stopped being whole,
/// the seeds included.
///
/// The mirror precondition holds: every seed must be a row that JUST stopped
/// being whole, and no seed may reference another seed, or a referrer is
/// incremented twice for one loss.
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

        let mut next = Vec::new();
        for row in rows {
            if row.try_get::<bool>("", flag)? {
                next.push(row.try_get::<String>("", "hash")?);
            }
        }

        all.extend(next.iter().cloned());
        frontier = next;
    }

    Ok(all)
}

/// What `retire_paths` removed and what stopped being whole because of it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Retired {
    pub deleted: Vec<String>,
    pub unwhole: Vec<String>,
}

/// Delete the `hashes` that satisfy `guard` from the index and move every
/// counter and flag that trusted them, in the caller's transaction: the reverse
/// ripple from the rows that were whole, `is_cached` off the outputs, and the
/// anchor flags the rows backed (`drv_closure_cached` on the owners of a deleted
/// `.drv`, `closure_complete` on the producers of every hash that stopped being
/// whole). `Retired::deleted` names the rows that actually went.
///
/// `guard` is an extra condition on the row being deleted (aliased `cp`), for a
/// caller that may drop a path only while something still holds - the TTL
/// eviction passes `NOT EXISTS (SELECT 1 FROM cached_path_signature ...)`, since
/// `cached_path_signature.cached_path` is `ON DELETE CASCADE` and a retire takes
/// every cache's signature with it. It belongs in the DELETE and nowhere else: a
/// separate `SELECT ... FOR UPDATE` that blocks on a concurrent commit's row lock
/// re-checks its condition through EvalPlanQual against the ORIGINAL statement
/// snapshot, so a signature that commit inserted for another cache is invisible,
/// the path is reported unsigned and retired, and the just-committed narinfo
/// starts 404ing. The DELETE's own snapshot is taken after the lock is granted
/// and sees it.
pub async fn retire_paths<C: ConnectionTrait>(
    db: &C,
    hashes: &[String],
    guard: Option<&str>,
) -> Result<Retired, DbErr> {
    if hashes.is_empty() {
        return Ok(Retired::default());
    }

    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            delete_statement(guard),
            [hashes.to_vec().into()],
        ))
        .await?;

    let mut deleted = Vec::with_capacity(rows.len());
    let mut were_whole = Vec::new();
    for row in rows {
        let hash = row.try_get::<String>("", "hash")?;
        if row.try_get::<bool>("", "was_whole")? {
            were_whole.push(hash.clone());
        }

        deleted.push(hash);
    }

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

    fn flag_row(flag: &str, value: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([(flag.to_owned(), Value::from(value))])
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
        assert!(
            sql.contains(&format!("AND NOT {}", whole_predicate("dep"))),
            "{sql}"
        );
        assert_eq!(
            norm(&whole_predicate("cp")),
            "(cp.file_hash IS NOT NULL AND cp.missing_references = 0)"
        );
    }

    /// The seed reports the state after the recount and nothing else: the commit
    /// has already stored the NAR by then, so no self-join can recover the
    /// pre-commit endpoint of the flip. Reading the flip from the row itself
    /// would report "did not become whole" for exactly the commit that must
    /// ripple (a fresh row is inserted backed, with the counter at its default
    /// zero), and every referrer of a re-pushed path would count it as missing
    /// forever. The caller pairs this with the pre-commit `is_whole()`.
    #[tokio::test]
    async fn the_seed_reports_the_state_and_leaves_the_transition_to_its_caller() {
        let sql = norm(SEED);
        assert!(!sql.contains("cached_path old"), "{sql}");
        assert!(sql.contains("WHERE cp.hash = $1"), "{sql}");
        assert!(
            sql.contains(&format!("RETURNING {} AS whole", whole_predicate("cp"))),
            "{sql}"
        );

        for whole in [true, false] {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![flag_row("whole", whole)]])
                .into_connection();

            assert_eq!(seed_references(&db, "h").await.unwrap(), whole);
        }

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        assert!(!seed_references(&db, "absent").await.unwrap());
    }

    /// Both ripples move a referrer once per edge into the frontier, so the
    /// per-referrer edge count and the self-exclusion are what keeps the counter
    /// sound.
    #[test]
    fn both_ripples_count_edges_per_referrer_and_exclude_self_references() {
        for sql in [norm(FORWARD), norm(REVERSE)] {
            assert!(
                sql.contains("count(*) AS n FROM cached_path_reference r"),
                "{sql}"
            );
            assert!(sql.contains("r.reference_hash = ANY($1)"), "{sql}");
            assert!(sql.contains("r.referrer <> r.reference_hash"), "{sql}");
            assert!(sql.contains("GROUP BY r.referrer"), "{sql}");
            assert!(sql.contains("WHERE cp.hash = c.referrer"), "{sql}");
        }

        assert!(norm(FORWARD).contains("SET missing_references = cp.missing_references - c.n"));
        assert!(norm(REVERSE).contains("SET missing_references = cp.missing_references + c.n"));
    }

    /// `RETURNING` sees the row after the update, so the seed, the forward ripple
    /// and the delete return the wholeness predicate itself, while the reverse
    /// ripple compares against `c.n`: the counter equals what was just added
    /// exactly when it was zero before, i.e. when the row was whole.
    #[test]
    fn every_returning_predicate_reads_the_one_wholeness_definition() {
        assert!(norm(SEED).contains(&format!("RETURNING {} AS whole", whole_predicate("cp"))));
        assert!(norm(FORWARD).contains(&format!(
            "RETURNING cp.hash, {} AS whole",
            whole_predicate("cp")
        )));
        assert!(norm(&delete_statement(None)).contains(&format!(
            "RETURNING cp.hash, {} AS was_whole",
            whole_predicate("cp")
        )));
        assert!(norm(REVERSE).contains(
            "RETURNING cp.hash, (cp.file_hash IS NOT NULL AND cp.missing_references = c.n) AS was_whole"
        ));
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
        assert_eq!(log.len(), 2, "one statement per level: {log:?}");
        assert!(log[0].contains("missing_references + c.n") && log[0].contains("\"gone\""));
        assert!(
            log[1].contains("\"r1\"") && !log[1].contains("\"r2\""),
            "{log:?}"
        );
    }

    /// A decode failure must not read as "not whole": that would truncate the
    /// ripple and leave referrers unwhole forever, with no sweep behind it.
    #[tokio::test]
    async fn a_ripple_row_that_does_not_decode_is_an_error_not_a_dead_end() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row("r1", "misspelled", true)]])
            .into_connection();

        assert!(ripple_whole(&db, vec!["seed".to_owned()]).await.is_err());
    }

    /// Retiring seeds the reverse ripple only from rows that were whole: a
    /// referrer of a row that was already incomplete counted it as missing
    /// already, so it must not be incremented twice. The flag clears then split:
    /// `is_cached` follows what was deleted, the anchor flags follow what stopped
    /// being whole.
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

        let retired = retire_paths(&db, &["a".to_owned(), "b".to_owned()], None)
            .await
            .unwrap();

        assert_eq!(retired.deleted, vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(retired.unwhole, vec!["a".to_owned()]);
        let log = statements(db);
        assert_eq!(
            log.len(),
            5,
            "delete, one ripple level, three flag clears: {log:?}"
        );
        assert!(log[0].contains("DELETE FROM cached_path") && log[0].contains("RETURNING"));
        assert!(
            log[1].contains("missing_references + c.n")
                && log[1].contains("\"a\"")
                && !log[1].contains("\"b\"")
        );
        assert!(
            log[2].contains("is_cached = false")
                && log[2].contains("\"a\"")
                && log[2].contains("\"b\""),
            "is_cached follows every deleted hash: {log:?}"
        );
        for clear in &log[3..] {
            assert!(
                clear.contains("\"a\"") && !clear.contains("\"b\""),
                "the anchor flags follow only what stopped being whole: {log:?}"
            );
        }

        assert!(log[3].contains("drv_closure_cached = false"));
        assert!(log[4].contains("closure_complete = false"));
    }

    /// Nothing to retire is a no-op: no statement at all.
    #[tokio::test]
    async fn retiring_nothing_issues_no_statement() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let retired = retire_paths(&db, &[], None).await.unwrap();
        assert!(retired.deleted.is_empty() && retired.unwhole.is_empty());
        assert!(statements(db).is_empty());
    }

    /// A caller's guard has to be ANDed into the DELETE itself. Checking it in a
    /// separate `SELECT ... FOR UPDATE` re-evaluates the condition through
    /// EvalPlanQual on the original statement snapshot once it blocks on a
    /// concurrent commit's row lock, so a `cached_path_signature` that commit
    /// inserted for another cache is invisible; the path is retired and the
    /// cascade takes the fresh signature with it, 404ing a narinfo committed
    /// seconds earlier. The DELETE's own snapshot is taken after the lock.
    #[tokio::test]
    async fn the_retire_guard_lands_inside_the_delete_statement() {
        let guard =
            "NOT EXISTS (SELECT 1 FROM cached_path_signature s WHERE s.cached_path = cp.id)";
        let sql = norm(&delete_statement(Some(guard)));
        assert!(
            sql.contains(&format!("WHERE cp.hash = ANY($1) AND ({guard})")),
            "{sql}"
        );

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                };
                3
            ])
            .into_connection();

        retire_paths(&db, &["a".to_owned()], Some(guard))
            .await
            .unwrap();

        let log = statements(db);
        assert!(
            log[0].contains("DELETE FROM cached_path") && log[0].contains("cached_path_signature"),
            "the guard must reach the delete: {log:?}"
        );
    }
}
