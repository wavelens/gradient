/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Runtime-closure walks over store-path references.
//!
//! Unlike the build closure (a walk of `derivation_dependency`), the runtime
//! closure follows the normalized `cached_path_reference` relation (referrer ->
//! referenced store hash) starting from a build's output store paths. It captures
//! exactly what a built artefact needs at runtime, and is only populated once
//! outputs are cached.

use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DbErr, EntityTrait, FromQueryResult,
    QueryFilter, Statement,
};
use std::collections::HashMap;

use gradient_types::*;

#[derive(FromQueryResult)]
struct ReferenceEdge {
    referrer: String,
    reference_hash: String,
}

#[derive(FromQueryResult)]
struct ReferenceToken {
    reference: String,
}

/// Extract the 32-char store hash from a `hash-name` reference token. Store
/// hashes are dash-free, so the hash is everything before the first `-`.
pub fn parse_reference_hash(reference: &str) -> Option<String> {
    let hash = reference.split('-').next().unwrap_or_default();
    (!hash.is_empty()).then(|| hash.to_string())
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

/// Runtime reference edges of `referrers`: `(referrer hash, referenced hash)`
/// pairs from `cached_path_reference`. The reverse index makes this an index
/// scan instead of parsing a text blob.
pub async fn reference_edges<C: ConnectionTrait>(
    db: &C,
    referrers: &[String],
) -> Result<Vec<(String, String)>, DbErr> {
    if referrers.is_empty() {
        return Ok(vec![]);
    }
    Ok(crate::fetch_in_chunks(referrers, |chunk| async move {
        ReferenceEdge::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT referrer, reference_hash FROM cached_path_reference WHERE referrer = ANY($1)",
            [chunk.into()],
        ))
        .all(db)
        .await
    })
    .await?
    .into_iter()
    .map(|e| (e.referrer, e.reference_hash))
    .collect())
}

/// Runtime references of `hash` as `hash-name` tokens in their stored order
/// (the order the worker sent them, i.e. nix `StorePathSet` / store-path order).
/// Used to reconstruct the narinfo `References:` line and signature fingerprint.
pub async fn references_for_hash<C: ConnectionTrait>(
    db: &C,
    hash: &str,
) -> Result<Vec<String>, DbErr> {
    Ok(
        ReferenceToken::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT reference FROM cached_path_reference WHERE referrer = $1 ORDER BY position",
            [hash.into()],
        ))
        .all(db)
        .await?
        .into_iter()
        .map(|r| r.reference)
        .collect(),
    )
}

/// Reference closure of `seed_hashes` as one recursive statement; returns every
/// reached `cached_path` row keyed by hash. Seeds and references without a
/// `cached_path` row (NAR not yet uploaded) are simply absent from the result.
pub async fn runtime_closure_reachable<C: ConnectionTrait>(
    db: &C,
    seed_hashes: &[String],
) -> Result<HashMap<String, gradient_entity::cached_path::Model>, DbErr> {
    if seed_hashes.is_empty() {
        return Ok(HashMap::new());
    }

    let sql = format!(
        "{} SELECT cp.* FROM cached_path cp JOIN refs r ON cp.hash = r.hash",
        crate::graph_sql::reference_closure_cte("refs", "SELECT unnest($1::text[])")
    );
    Ok(ECachedPath::find()
        .from_raw_sql(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            [seed_hashes.to_vec().into()],
        ))
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.hash.clone(), row))
        .collect())
}

/// Store paths in the reference closure of `seed_hashes` that this cache can
/// actually serve, excluding the seeds themselves and capped at `limit`.
///
/// Answers "what else will the caller need, and can we hand it over now" in one
/// statement, so a worker learns a whole closure per round trip instead of one
/// hop at a time. Only backed rows come back, so a caller may treat every
/// returned path as serveable. The walk dedupes on `hash` alone: adding depth to
/// the key would let a diamond re-enter the frontier and never terminate.
pub async fn runtime_closure_cached_paths<C: ConnectionTrait>(
    db: &C,
    seed_hashes: &[String],
    limit: u64,
) -> Result<Vec<String>, DbErr> {
    if seed_hashes.is_empty() || limit == 0 {
        return Ok(vec![]);
    }

    let sql = format!(
        "{} SELECT '/nix/store/' || cp.hash || '-' || cp.package AS reference \
         FROM cached_path cp JOIN refs r ON cp.hash = r.hash \
         WHERE cp.file_hash IS NOT NULL AND cp.hash <> ALL($1::text[]) \
         LIMIT $2",
        crate::graph_sql::reference_closure_cte("refs", "SELECT unnest($1::text[])")
    );
    Ok(
        ReferenceToken::find_by_statement(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            sql,
            [seed_hashes.to_vec().into(), (limit as i64).into()],
        ))
        .all(db)
        .await?
        .into_iter()
        .map(|r| r.reference)
        .collect(),
    )
}

/// Total NAR size of the runtime closure seeded at `seed_hashes`.
pub async fn runtime_closure_size<C: ConnectionTrait>(
    db: &C,
    seed_hashes: &[String],
) -> Result<i64, DbErr> {
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

    /// Both degenerate inputs must short-circuit before issuing SQL: an
    /// unseeded MockDatabase errors on any query, so reaching one fails here.
    #[tokio::test]
    async fn closure_expansion_skips_the_query_when_there_is_nothing_to_ask() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        assert!(
            runtime_closure_cached_paths(&db, &[], 100)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            runtime_closure_cached_paths(&db, &["abc".to_string()], 0)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// The walk must dedupe on `hash` alone. Keying on anything that varies per
    /// visit (a depth counter) lets a diamond re-enter the frontier forever.
    #[test]
    fn closure_expansion_walks_fenced_and_dedupes_on_hash_alone() {
        let cte = crate::graph_sql::reference_closure_cte("refs", "SELECT unnest($1::text[])");
        assert!(cte.contains("refs(hash)"), "dedup key is the hash: {cte}");
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
