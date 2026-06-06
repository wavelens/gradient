/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Runtime-closure walks over store-path references.
//!
//! Unlike the build closure (a walk of `derivation_dependency`), the runtime
//! closure follows `cached_path.references` - the narinfo `References:` field -
//! starting from a build's output store paths. It captures exactly what a built
//! artefact needs at runtime, and is only populated once outputs are cached.

use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use std::collections::{HashMap, HashSet};

use crate::types::*;

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
    Ok(EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.is_in(drv_ids.to_vec()))
        .all(db)
        .await?
        .into_iter()
        .map(|o| o.hash)
        .collect())
}

/// BFS over `cached_path.references` from `seed_hashes`; returns every reached
/// `cached_path` row keyed by hash. Seeds and references without a `cached_path`
/// row (NAR not yet uploaded) are simply absent from the result.
pub async fn runtime_closure_reachable<C: ConnectionTrait>(
    db: &C,
    seed_hashes: &[String],
) -> Result<HashMap<String, entity::cached_path::Model>, DbErr> {
    let mut reached: HashMap<String, entity::cached_path::Model> = HashMap::new();
    let mut visited: HashSet<String> = seed_hashes.iter().cloned().collect();
    let mut frontier: Vec<String> = seed_hashes.to_vec();

    while !frontier.is_empty() {
        let rows = ECachedPath::find()
            .filter(CCachedPath::Hash.is_in(frontier.clone()))
            .all(db)
            .await?;
        frontier.clear();
        for row in rows {
            for token in row.references.clone().unwrap_or_default().split_whitespace() {
                if let Some(hash) = parse_reference_hash(token)
                    && visited.insert(hash.clone())
                {
                    frontier.push(hash);
                }
            }
            reached.insert(row.hash.clone(), row);
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
    use entity::cached_path;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn now() -> chrono::NaiveDateTime {
        chrono::Utc::now().naive_utc()
    }

    fn cp(hash: &str, nar_size: i64, references: &str) -> cached_path::Model {
        cached_path::Model {
            id: CachedPathId::now_v7(),
            store_path: format!("/nix/store/{hash}-foo"),
            hash: hash.into(),
            package: "foo".into(),
            file_hash: Some("sha256:dummy".into()),
            file_size: Some(nar_size / 2),
            nar_size: Some(nar_size),
            nar_hash: None,
            references: (!references.is_empty()).then(|| references.to_string()),
            ca: None,
            deriver: None,
            created_at: now(),
        }
    }

    #[test]
    fn reference_hash_strips_name() {
        assert_eq!(parse_reference_hash("abc123-hello-2.10").as_deref(), Some("abc123"));
        assert_eq!(parse_reference_hash("abc123").as_deref(), Some("abc123"));
        assert_eq!(parse_reference_hash(""), None);
    }

    // root -refs-> child (size 100 + 40) => total 140 over two reached rows.
    #[tokio::test]
    async fn walks_references_and_sums() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![cp("root", 100, "child-foo")]])
            .append_query_results([vec![cp("child", 40, "")]])
            .into_connection();

        let reached = runtime_closure_reachable(&db, &["root".into()]).await.unwrap();
        assert_eq!(reached.len(), 2);
        assert_eq!(reached.values().filter_map(|r| r.nar_size).sum::<i64>(), 140);
    }

    // root -> a, root -> b, a -> c, b -> c: c counted once.
    #[tokio::test]
    async fn dedups_diamond() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![cp("root", 10, "a-foo b-foo")]])
            .append_query_results([vec![cp("a", 20, "c-foo"), cp("b", 30, "c-foo")]])
            .append_query_results([vec![cp("c", 40, "")]])
            .into_connection();

        let total = runtime_closure_size(&db, &["root".into()]).await.unwrap();
        assert_eq!(total, 100);
    }

    #[tokio::test]
    async fn empty_seeds_is_zero() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        assert_eq!(runtime_closure_size(&db, &[]).await.unwrap(), 0);
    }
}
