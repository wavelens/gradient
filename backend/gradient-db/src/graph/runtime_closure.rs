/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, DbErr, EntityTrait, FromQueryResult,
    QueryFilter, TransactionTrait,
};
use std::collections::HashMap;

use gradient_types::*;

#[derive(FromQueryResult)]
struct ReferenceTokens {
    hash: String,
    references: Option<String>,
}

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

pub async fn reference_edges<C: ConnectionTrait>(
    db: &C,
    wanted_by: &[String],
) -> Result<Vec<(String, String)>, DbErr> {
    let mut edges = Vec::new();
    for (parent, refs) in references_for_hashes(db, wanted_by).await? {
        for token in refs {
            if let Some(hash) = parse_reference_hash(&token) {
                edges.push((parent.clone(), hash));
            }
        }
    }

    Ok(edges)
}

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

pub async fn references_for_hashes<C: ConnectionTrait>(
    db: &C,
    hashes: &[String],
) -> Result<HashMap<String, Vec<String>>, DbErr> {
    if hashes.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = crate::fetch_in_chunks(hashes, |chunk| async move {
        ReferenceTokens::find_by_statement(REFERENCES_FOR_HASHES.bind([chunk.into()]))
            .all(db)
            .await
    })
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| (row.hash, tokens(row.references)))
        .collect())
}

/// The path lookup is fenced like the walk.
/// A plain join is giving the planner no closure size.
/// The planner would then hash-join every path in the cache.
fn runtime_closure_reachable_sql() -> String {
    format!(
        "{cte} \
         SELECT s.* FROM (\
         SELECT o.hash FROM derivation_output o JOIN runtime t ON t.derivation = o.derivation \
         UNION SELECT u.h FROM unnest($1::text[]) AS u(h)) r, \
         LATERAL (SELECT cp.* FROM cached_path cp WHERE cp.hash = r.hash OFFSET 0) s",
        cte = crate::graph::walks::runtime_closure_cte(
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

    let walk = crate::graph::walks::begin_walk(db).await?;
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

    #[tokio::test]
    async fn empty_seeds_is_zero() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        assert_eq!(runtime_closure_size(&db, &[]).await.unwrap(), 0);
    }

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
        assert!(
            log[0].contains(crate::graph::walks::WALK_WORK_MEM),
            "{log:?}"
        );
        assert!(log[1].contains("derivation_dependency"), "{log:?}");
    }

    /// The walk must dedupe on the derivation alone.
    /// A per-visit key like a depth counter would let a diamond re-enter the frontier forever.
    #[test]
    fn closure_expansion_walks_fenced_and_dedupes_on_the_node_alone() {
        let cte = crate::graph::walks::runtime_closure_cte("refs", "SELECT $1::uuid");
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
