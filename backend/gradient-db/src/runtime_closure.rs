/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Runtime-closure walks, and the narinfo `References:` line they are built from.
//!
//! Build-time and runtime dependencies are two kinds of one edge, so the runtime
//! closure is the same `derivation_dependency` walk as the build closure with
//! `kind IN (1, 2)` instead of `kind IN (0, 2)`. The ordered reference tokens of a
//! single path live in `cached_path.references`, which is what the narinfo line and
//! the signature fingerprint are reconstructed from verbatim.

use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, DbErr, EntityTrait, FromQueryResult,
    QueryFilter, TransactionTrait,
};
use std::collections::HashMap;

use gradient_types::*;

#[derive(FromQueryResult)]
struct ReferrerTokens {
    hash: String,
    references: Option<String>,
}

/// Extract the 32-char store hash from a `hash-name` reference token. Store
/// hashes are dash-free, so the hash is everything before the first `-`.
pub fn parse_reference_hash(reference: &str) -> Option<String> {
    let hash = reference.split('-').next().unwrap_or_default();
    (!hash.is_empty()).then(|| hash.to_string())
}

fn tokens(references: Option<String>) -> Vec<String> {
    references
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// Output store-path hashes of `drv_ids`, the seeds of their runtime closures.
pub async fn output_hashes_for_drvs<C: ConnectionTrait>(
    db: &C,
    drv_ids: &[DerivationId],
) -> Result<Vec<String>, DbErr> {
    if drv_ids.is_empty() {
        return Ok(vec![]);
    }
    Ok(crate::fetch_in_chunks(drv_ids, |chunk| async move {
        EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await?
    .into_iter()
    .map(|o| o.hash)
    .collect())
}

crate::sql! {
    REFERENCES_FOR_HASHES = "SELECT hash, \"references\" FROM cached_path WHERE hash = ANY($1)",
        params = [CachedPathHashes(64)];
}

/// Runtime reference edges of `referrers`: `(referrer hash, referenced hash)`
/// pairs, read off the narinfo line each referrer stores.
pub async fn reference_edges<C: ConnectionTrait>(
    db: &C,
    referrers: &[String],
) -> Result<Vec<(String, String)>, DbErr> {
    let mut edges = Vec::new();
    for (referrer, refs) in references_for_hashes(db, referrers).await? {
        for token in refs {
            if let Some(hash) = parse_reference_hash(&token) {
                edges.push((referrer.clone(), hash));
            }
        }
    }

    Ok(edges)
}

/// Runtime references of `hash` as `hash-name` tokens in their stored order
/// (the order the worker sent them, i.e. nix `StorePathSet` / store-path order).
/// Used to reconstruct the narinfo `References:` line and signature fingerprint.
pub async fn references_for_hash<C: ConnectionTrait>(
    db: &C,
    hash: &str,
) -> Result<Vec<String>, DbErr> {
    Ok(
        references_for_hashes(db, std::slice::from_ref(&hash.to_owned()))
            .await?
            .remove(hash)
            .unwrap_or_default(),
    )
}

/// [`references_for_hash`] for many referrers at once, grouped by referrer and
/// kept in stored order within each group.
///
/// Answering a `Pull` cache query one path at a time is what made a full-width
/// query miss its deadline: a reply reaches `CACHE_QUERY_MAX_PATHS`, so a
/// per-path lookup is up to a thousand sequential round trips before the first
/// byte of the reply.
pub async fn references_for_hashes<C: ConnectionTrait>(
    db: &C,
    hashes: &[String],
) -> Result<HashMap<String, Vec<String>>, DbErr> {
    if hashes.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = crate::fetch_in_chunks(hashes, |chunk| async move {
        ReferrerTokens::find_by_statement(REFERENCES_FOR_HASHES.bind([chunk.into()]))
            .all(db)
            .await
    })
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| (row.hash, tokens(row.references)))
        .collect())
}

/// The reached derivations' outputs, plus the seeds themselves: a seed whose own
/// producer is unknown still has a `cached_path` row worth reporting. The path
/// lookup is fenced like the walk above it, because joining the closure to
/// `cached_path` plainly gives the planner no size for the closure and it answers
/// with a hash join over every path in the cache.
fn runtime_closure_reachable_sql() -> String {
    format!(
        "{cte} \
         SELECT s.* FROM (\
         SELECT o.hash FROM derivation_output o JOIN runtime t ON t.derivation = o.derivation \
         UNION SELECT u.h FROM unnest($1::text[]) AS u(h)) r, \
         LATERAL (SELECT cp.* FROM cached_path cp WHERE cp.hash = r.hash OFFSET 0) s",
        cte = crate::graph_sql::runtime_closure_cte(
            "runtime",
            "SELECT o.derivation FROM derivation_output o WHERE o.hash = ANY($1::text[])",
        ),
    )
}

crate::sql_fn! {
    RUNTIME_CLOSURE_REACHABLE = runtime_closure_reachable_sql,
        params = [CachedPathHashes(64)],
        tier = Walk,
        flags = [Walk];
}

/// Runtime closure of `seed_hashes` as one recursive statement; returns every
/// reached `cached_path` row keyed by hash. Seeds and references without a
/// `cached_path` row (NAR not yet uploaded) are simply absent from the result.
pub async fn runtime_closure_reachable<C>(
    db: &C,
    seed_hashes: &[String],
) -> Result<HashMap<String, gradient_entity::cached_path::Model>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    if seed_hashes.is_empty() {
        return Ok(HashMap::new());
    }

    let walk = crate::graph_sql::begin_walk(db).await?;
    let reached: HashMap<String, gradient_entity::cached_path::Model> = ECachedPath::find()
        .from_raw_sql(RUNTIME_CLOSURE_REACHABLE.bind([seed_hashes.to_vec().into()]))
        .all(&walk)
        .await?
        .into_iter()
        .map(|row| (row.hash.clone(), row))
        .collect();
    walk.commit().await?;

    Ok(reached)
}

/// Total NAR size of the runtime closure seeded at `seed_hashes`.
pub async fn runtime_closure_size<C>(db: &C, seed_hashes: &[String]) -> Result<i64, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let reached = runtime_closure_reachable(db, seed_hashes).await?;
    Ok(reached.values().filter_map(|r| r.nar_size).sum())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[test]
    fn reference_hash_strips_name() {
        assert_eq!(
            parse_reference_hash("abc123-hello-2.10").as_deref(),
            Some("abc123")
        );
        assert_eq!(parse_reference_hash("abc123").as_deref(), Some("abc123"));
        assert_eq!(parse_reference_hash(""), None);
    }

    // Empty seeds never query and sum to zero. The walk itself is one recursive
    // statement now, so its behaviour is Postgres's; the cache integration test
    // covers it end to end.
    #[tokio::test]
    async fn empty_seeds_is_zero() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        assert_eq!(runtime_closure_size(&db, &[]).await.unwrap(), 0);
    }

    /// The narinfo line is one column on the path, and the closure is a walk of
    /// the graph's runtime edges. Neither reads the retired path-level index.
    #[test]
    fn the_line_is_a_column_and_the_closure_is_a_graph_walk() {
        let refs = REFERENCES_FOR_HASHES.text();
        assert!(
            refs.contains("SELECT hash, \"references\" FROM cached_path"),
            "{refs}"
        );
        let walk = RUNTIME_CLOSURE_REACHABLE.text();
        assert!(walk.contains("e.kind IN (1, 2)"), "{walk}");
        assert!(!walk.contains("cached_path_reference"), "{walk}");
    }

    /// The runtime closure is the widest read in the system, so it runs inside
    /// the raised-`work_mem` transaction instead of on the bare pool where
    /// `SET LOCAL` would be ignored.
    #[tokio::test]
    async fn the_reference_walk_runs_inside_the_raised_work_mem_transaction() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .into_connection();
        assert!(
            runtime_closure_reachable(&db, &["abc".to_string()])
                .await
                .expect("the walk runs")
                .is_empty()
        );

        let log = crate::pool::statements(db.into_transaction_log());
        assert_eq!(
            log.len(),
            2,
            "the raise and the walk, in that order: {log:?}"
        );
        assert!(log[0].contains(crate::graph_sql::WALK_WORK_MEM), "{log:?}");
        assert!(log[1].contains("derivation_dependency"), "{log:?}");
    }

    /// The walk must dedupe on the derivation alone. Keying on anything that
    /// varies per visit (a depth counter) lets a diamond re-enter the frontier
    /// forever.
    #[test]
    fn closure_expansion_walks_fenced_and_dedupes_on_the_node_alone() {
        let cte = crate::graph_sql::runtime_closure_cte("refs", "SELECT $1::uuid");
        assert!(
            cte.contains("refs(derivation)"),
            "dedup key is the node: {cte}"
        );
        assert!(
            cte.contains("OFFSET 0) s"),
            "recursive term stays fenced: {cte}"
        );
        assert!(
            !cte.contains("UNION ALL"),
            "UNION ALL never terminates here: {cte}"
        );
    }
}
