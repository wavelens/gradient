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
use std::collections::{HashMap, HashSet};

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

/// BFS over `cached_path_reference` from `seed_hashes`; returns every reached
/// `cached_path` row keyed by hash. Seeds and references without a `cached_path`
/// row (NAR not yet uploaded) are simply absent from the result.
pub async fn runtime_closure_reachable<C: ConnectionTrait>(
    db: &C,
    seed_hashes: &[String],
) -> Result<HashMap<String, gradient_entity::cached_path::Model>, DbErr> {
    let mut reached: HashMap<String, gradient_entity::cached_path::Model> = HashMap::new();
    let mut visited: HashSet<String> = seed_hashes.iter().cloned().collect();
    let mut frontier: Vec<String> = seed_hashes.to_vec();

    while !frontier.is_empty() {
        let rows = crate::fetch_in_chunks(&frontier, |chunk| async move {
            ECachedPath::find()
                .filter(CCachedPath::Hash.is_in(chunk))
                .all(db)
                .await
        })
        .await?;
        let edges = reference_edges(db, &frontier).await?;
        frontier.clear();

        for row in rows {
            reached.insert(row.hash.clone(), row);
        }
        for (_, reference_hash) in edges {
            if visited.insert(reference_hash.clone()) {
                frontier.push(reference_hash);
            }
        }
    }

    Ok(reached)
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

    // Empty seeds never query and sum to zero. The non-trivial walk over
    // `cached_path_reference` is covered end-to-end by the cache integration test
    // (MockDatabase cannot represent the per-level model + edge queries).
    #[tokio::test]
    async fn empty_seeds_is_zero() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        assert_eq!(runtime_closure_size(&db, &[]).await.unwrap(), 0);
    }
}
