/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::walks::{ClosureDirection, dependency_closure_cte};
use crate::store_path_content_variant_reader_choice::{
    StorePathContentVariantReader,
    get_best_ranked_stored_copy_in_any_cache_for_each_store_path_hash,
    get_trusted_content_variant_for_each_store_path_hash_for_project_shared_build_or_binary_cache,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, DbErr, EntityTrait, FromQueryResult,
    QueryFilter, TransactionTrait,
};
use std::collections::{HashMap, HashSet};

use gradient_types::*;

#[derive(FromQueryResult)]
struct DerivationRow {
    derivation: uuid::Uuid,
}

#[derive(FromQueryResult)]
struct EdgeRow {
    derivation: uuid::Uuid,
    dependency: uuid::Uuid,
}

fn roots_closure_cte() -> String {
    dependency_closure_cte(
        "closure",
        "SELECT unnest($1::uuid[])",
        ClosureDirection::Dependencies,
    )
}

fn transitive_closure_reachable_sql() -> String {
    format!("{} SELECT derivation FROM closure", roots_closure_cte())
}

crate::sql_fn! {
    TRANSITIVE_CLOSURE_REACHABLE = transitive_closure_reachable_sql,
        params = [DerivationIds(64)],
        tier = Walk,
        flags = [Walk];
}

/// The edge lookup is fenced like the walk.
/// A plain join is giving the planner no closure size.
/// The planner would then hash-join every edge in the database.
fn transitive_closure_edges_sql() -> String {
    format!(
        "{} SELECT s.derivation, s.dependency FROM closure c, \
         LATERAL (SELECT e.derivation, e.dependency FROM derivation_dependency e \
                  WHERE e.derivation = c.derivation OFFSET 0) s",
        roots_closure_cte()
    )
}

crate::sql_fn! {
    TRANSITIVE_CLOSURE_EDGES = transitive_closure_edges_sql,
        params = [DerivationIds(64)],
        tier = Walk,
        flags = [Walk];
}

pub async fn transitive_closure_reachable<C>(
    db: &C,
    roots: &[DerivationId],
) -> Result<HashSet<DerivationId>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    if roots.is_empty() {
        return Ok(HashSet::new());
    }

    let walk = crate::graph::walks::begin_walk(db, &TRANSITIVE_CLOSURE_REACHABLE).await?;
    let reached = transitive_closure_reachable_in(&walk, roots).await?;
    walk.commit().await?;

    Ok(reached)
}

pub async fn transitive_closure_reachable_in(
    walk: &DatabaseTransaction,
    roots: &[DerivationId],
) -> Result<HashSet<DerivationId>, DbErr> {
    if roots.is_empty() {
        return Ok(HashSet::new());
    }

    let ids: Vec<uuid::Uuid> = roots.iter().map(|d| d.into_inner()).collect();
    let reached = DerivationRow::find_by_statement(TRANSITIVE_CLOSURE_REACHABLE.bind([ids.into()]))
        .all(walk)
        .await?
        .into_iter()
        .map(|r| DerivationId::new(r.derivation))
        .collect();

    Ok(reached)
}

pub async fn output_sizes_by_drv<C: ConnectionTrait>(
    db: &C,
    reader: StorePathContentVariantReader,
    drv_ids: &[DerivationId],
) -> Result<HashMap<DerivationId, i64>, DbErr> {
    let outputs = load_derivation_outputs_for_derivation_ids_in_chunks(db, drv_ids).await?;
    let copies =
        get_trusted_content_variant_for_each_store_path_hash_for_project_shared_build_or_binary_cache(
            db,
            reader,
            &distinct_output_hashes_without_nar_size(&outputs),
        )
        .await?;

    Ok(
        sum_output_sizes_per_derivation_from_output_rows_and_copy_size_by_store_path_hash(
            outputs, &copies,
        ),
    )
}

async fn output_sizes_by_drv_from_best_ranked_copy_in_any_cache<C: ConnectionTrait>(
    db: &C,
    drv_ids: &[DerivationId],
) -> Result<HashMap<DerivationId, i64>, DbErr> {
    let outputs = load_derivation_outputs_for_derivation_ids_in_chunks(db, drv_ids).await?;
    let copies = get_best_ranked_stored_copy_in_any_cache_for_each_store_path_hash(
        db,
        &distinct_output_hashes_without_nar_size(&outputs),
    )
    .await?;

    Ok(
        sum_output_sizes_per_derivation_from_output_rows_and_copy_size_by_store_path_hash(
            outputs, &copies,
        ),
    )
}

fn distinct_output_hashes_without_nar_size(outputs: &[MDerivationOutput]) -> Vec<String> {
    outputs
        .iter()
        .filter(|o| o.nar_size.is_none())
        .filter_map(|o| o.hash.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
}

fn sum_output_sizes_per_derivation_from_output_rows_and_copy_size_by_store_path_hash(
    outputs: Vec<MDerivationOutput>,
    copies: &HashMap<String, MCachedPath>,
) -> HashMap<DerivationId, i64> {
    let mut by_drv: HashMap<DerivationId, i64> = HashMap::new();
    for o in outputs {
        let copy_size = || Some(copies.get(o.hash.as_ref()?)?.nar_size);
        if let Some(size) = o.nar_size.or_else(copy_size) {
            *by_drv.entry(o.derivation).or_insert(0) += size;
        }
    }
    by_drv
}

async fn load_derivation_outputs_for_derivation_ids_in_chunks<C: ConnectionTrait>(
    db: &C,
    drv_ids: &[DerivationId],
) -> Result<Vec<MDerivationOutput>, DbErr> {
    crate::fetch_in_chunks(drv_ids, |chunk| async move {
        EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await
}

pub async fn transitive_closure_sizes<C>(
    db: &C,
    roots: &[DerivationId],
) -> Result<HashMap<DerivationId, i64>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    if roots.is_empty() {
        return Ok(HashMap::new());
    }

    let ids: Vec<uuid::Uuid> = roots.iter().map(|d| d.into_inner()).collect();
    let walk = crate::graph::walks::begin_walk(db, &TRANSITIVE_CLOSURE_EDGES).await?;
    let edges = EdgeRow::find_by_statement(TRANSITIVE_CLOSURE_EDGES.bind([ids.into()]))
        .all(&walk)
        .await?;
    walk.commit().await?;

    let edges: Vec<(DerivationId, DerivationId)> = edges
        .into_iter()
        .map(|edge| {
            (
                DerivationId::new(edge.derivation),
                DerivationId::new(edge.dependency),
            )
        })
        .collect();
    let reachable: Vec<DerivationId> = roots
        .iter()
        .copied()
        .chain(edges.iter().flat_map(|&(from, to)| [from, to]))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let size_by_drv =
        output_sizes_by_drv_from_best_ranked_copy_in_any_cache(db, &reachable).await?;

    Ok(transitive_closure_output_sizes(roots, &edges, &size_by_drv))
}

fn transitive_closure_output_sizes(
    roots: &[DerivationId],
    edges: &[(DerivationId, DerivationId)],
    size_by_drv: &HashMap<DerivationId, i64>,
) -> HashMap<DerivationId, i64> {
    let mut index_of: HashMap<DerivationId, usize> = HashMap::new();
    let nodes = roots
        .iter()
        .copied()
        .chain(edges.iter().flat_map(|&(from, to)| [from, to]));
    for drv in nodes {
        let next = index_of.len();
        index_of.entry(drv).or_insert(next);
    }

    let mut dependencies: Vec<Vec<usize>> = vec![Vec::new(); index_of.len()];
    for (from, to) in edges {
        dependencies[index_of[from]].push(index_of[to]);
    }
    let mut sizes = vec![0i64; index_of.len()];
    for (drv, &index) in &index_of {
        sizes[index] = size_by_drv.get(drv).copied().unwrap_or(0);
    }

    let mut last_visiting_root = vec![usize::MAX; index_of.len()];
    roots
        .iter()
        .enumerate()
        .map(|(root_number, root)| {
            let start = index_of[root];
            last_visiting_root[start] = root_number;
            let mut stack = vec![start];
            let mut total = 0;
            while let Some(node) = stack.pop() {
                total += sizes[node];
                for &dependency in &dependencies[node] {
                    if last_visiting_root[dependency] != root_number {
                        last_visiting_root[dependency] = root_number;
                        stack.push(dependency);
                    }
                }
            }
            (*root, total)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::runtime_closure::runtime_closure_reachable;
    use crate::graph::sbom::with_referenced_paths;
    use crate::store_path_content_variant_reader_choice::StorePathContentVariantReader;
    use gradient_fixtures::mock::{empty_db, mock_db};
    use gradient_fixtures::rows::{derivation_dependency, derivation_output};
    use gradient_fixtures::store_path_content_variants::{
        BUILD_A, BUILD_B, FIRST_KEY, Readers, SECOND_KEY, SHARED,
        two_variants_of_one_hash_and_one_untrusted_hash,
    };

    fn raise() -> sea_orm::MockExecResult {
        sea_orm::MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }
    }

    #[tokio::test]
    async fn empty_roots_is_zero() {
        let db = empty_db();
        let sizes = transitive_closure_sizes(&db, &[]).await.unwrap();
        assert!(sizes.is_empty());
    }

    #[tokio::test]
    async fn bulk_sizes_dedup_diamond() {
        let root = DerivationId::now_v7();
        let a = DerivationId::now_v7();
        let b = DerivationId::now_v7();
        let c = DerivationId::now_v7();
        let db = mock_db()
            .append_exec_results([raise()])
            .append_query_results([vec![
                derivation_dependency(root, a),
                derivation_dependency(root, b),
                derivation_dependency(a, c),
                derivation_dependency(b, c),
            ]])
            .append_query_results([vec![
                derivation_output(root, "r", Some(10)),
                derivation_output(a, "a", Some(20)),
                derivation_output(b, "b", Some(30)),
                derivation_output(c, "c", Some(40)),
            ]])
            .into_connection();

        let sizes = transitive_closure_sizes(&db, &[root]).await.unwrap();
        assert_eq!(sizes.get(&root).copied(), Some(100));
    }

    #[tokio::test]
    async fn shared_dependency_size_looked_up_once_and_counted_for_both_roots() {
        let first = DerivationId::now_v7();
        let second = DerivationId::now_v7();
        let shared = DerivationId::now_v7();
        let db = mock_db()
            .append_exec_results([raise()])
            .append_query_results([vec![
                derivation_dependency(first, shared),
                derivation_dependency(second, shared),
            ]])
            .append_query_results([vec![
                derivation_output(first, "first", Some(1)),
                derivation_output(second, "second", Some(2)),
                derivation_output(shared, "shared", None),
            ]])
            .append_query_results([vec![MCachedPath {
                hash: "shared".into(),
                nar_size: 40,
                ..Default::default()
            }]])
            .into_connection();

        let sizes = transitive_closure_sizes(&db, &[first, second])
            .await
            .unwrap();

        assert_eq!(sizes, HashMap::from([(first, 41), (second, 42)]));
        let stored_copy_reads: Vec<_> = db
            .into_transaction_log()
            .into_iter()
            .flat_map(|transaction| transaction.statements().to_vec())
            .filter(|statement| statement.sql.contains("FROM cached_path"))
            .map(|statement| statement.values)
            .collect();
        assert_eq!(
            stored_copy_reads,
            vec![Some(sea_orm::Values(vec![
                vec!["shared".to_owned()].into()
            ]))]
        );
    }

    #[tokio::test]
    async fn one_raise_serves_many_root_sets_on_the_same_walk() {
        let a = DerivationId::now_v7();
        let b = DerivationId::now_v7();
        let db = mock_db()
            .append_exec_results([raise()])
            .append_query_results([vec![derivation_dependency(a, a)]])
            .append_query_results([vec![derivation_dependency(b, b)]])
            .into_connection();

        let walk = crate::graph::walks::begin_walk(&db, &TRANSITIVE_CLOSURE_REACHABLE)
            .await
            .unwrap();
        let first = transitive_closure_reachable_in(&walk, &[a]).await.unwrap();
        let second = transitive_closure_reachable_in(&walk, &[b]).await.unwrap();
        walk.commit().await.unwrap();

        assert_eq!(first, HashSet::from([a]));
        assert_eq!(second, HashSet::from([b]));
    }

    async fn insert_two_content_variants_with_derivation_output()
    -> (Readers, DerivationId, DerivationId) {
        let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
        let derivation_a = readers.db.shared_build(BUILD_A).await.derivation;
        let derivation_b = readers.db.shared_build(BUILD_B).await.derivation;
        readers
            .db
            .execute(&format!(
                "UPDATE cached_path SET \
                   nar_size = CASE WHEN nar_object_storage_key = '{FIRST_KEY}' THEN 100 ELSE 200 END, \
                   \"references\" = CASE WHEN nar_object_storage_key = '{FIRST_KEY}' \
                                    THEN '{SHARED}-first' ELSE '{SHARED}-second' END \
                 WHERE hash = '{SHARED}'; \
                 INSERT INTO derivation_output \
                   (id, derivation, name, hash, package, path_known_after_build, created_at) \
                 VALUES (uuidv7(), '{derivation_a}', 'out', '{SHARED}', '{SHARED}-shared', false, now()), \
                        (uuidv7(), '{derivation_b}', 'out', '{SHARED}', '{SHARED}-shared', false, now());"
            ))
            .await;
        (readers, derivation_a, derivation_b)
    }

    fn optional_path_row_to_storage_key_nar_size_and_references(
        path: Option<&MCachedPath>,
    ) -> Option<(String, i64, Option<String>)> {
        path.map(|path| {
            (
                path.nar_object_storage_key.clone(),
                path.nar_size,
                path.references.clone(),
            )
        })
    }

    #[tokio::test]
    async fn runtime_closure_output_sizes_and_sbom_reference_completion_use_content_variant_trusted_by_reading_project_for_shared_store_path_hash()
     {
        let (readers, derivation_a, derivation_b) =
            insert_two_content_variants_with_derivation_output().await;
        let conn = readers.db.connection();
        let seeds = [SHARED.to_owned()];

        let mut seen = Vec::new();
        for (project, derivation) in [
            (readers.project_a, derivation_a),
            (readers.project_b, derivation_b),
        ] {
            let reader = StorePathContentVariantReader::Project(project);
            let closure = runtime_closure_reachable(&conn, reader.clone(), &seeds)
                .await
                .expect("The runtime closure walk ran.");
            let referencing = MCachedPath {
                hash: "root".into(),
                references: Some(format!("{SHARED}-shared")),
                ..Default::default()
            };
            let completed = with_referenced_paths(
                &conn,
                reader.clone(),
                HashMap::from([("root".into(), referencing)]),
            )
            .await
            .expect("The SBOM reference completion ran.");
            let sizes = output_sizes_by_drv(&conn, reader, &[derivation])
                .await
                .expect("The output size read ran.");
            seen.push((
                optional_path_row_to_storage_key_nar_size_and_references(closure.get(SHARED)),
                optional_path_row_to_storage_key_nar_size_and_references(completed.get(SHARED)),
                sizes.get(&derivation).copied(),
            ));
        }

        let first = Some((FIRST_KEY.to_owned(), 100, Some(format!("{SHARED}-first"))));
        let second = Some((SECOND_KEY.to_owned(), 200, Some(format!("{SHARED}-second"))));
        assert_eq!(
            seen,
            vec![
                (first.clone(), first, Some(100)),
                (second.clone(), second, Some(200)),
            ],
            "Projects read only the content variant that they trust. Content variants contain their own row, references and size."
        );
        readers.db.finish().await;
    }

    #[tokio::test]
    async fn scheduler_closure_sizes_per_root_derivation_use_worker_built_dependency_output_copy_for_shared_builds_when_hash_has_worker_built_and_trusted_uploaded_copies()
     {
        let (readers, derivation_a, derivation_b) =
            insert_two_content_variants_with_derivation_output().await;
        readers
            .db
            .execute(&format!(
                "UPDATE cache SET private_key = 'signing-key' \
                 WHERE id IN ('{}', '{}');",
                readers.cache_one, readers.cache_two
            ))
            .await;

        let sizes =
            transitive_closure_sizes(&readers.db.connection(), &[derivation_a, derivation_b])
                .await
                .expect("The closure size read ran.");

        assert_eq!(
            (
                sizes.get(&derivation_a).copied(),
                sizes.get(&derivation_b).copied()
            ),
            (Some(100), Some(100)),
            "A shared build can put the worker-built content variant ahead of the variant that a project uploaded in the estimate of the size of dependency outputs. That order can apply even when a project can trust its own upload."
        );
        readers.db.finish().await;
    }
}
