/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, DbErr, EntityTrait, FromQueryResult,
    QueryFilter, TransactionTrait,
};
use std::collections::{HashMap, VecDeque};

use crate::store_path_content_variant_reader_choice::{
    StorePathContentVariantReader,
    get_trusted_content_variant_for_each_store_path_hash_for_project_shared_build_or_binary_cache,
};
use gradient_types::*;

#[derive(FromQueryResult)]
struct RuntimeClosureStorePathHash {
    hash: String,
}

pub fn parse_reference_hash(reference: &str) -> Option<String> {
    StorePath::parse_stored(reference)
        .ok()
        .map(|path| path.hash().to_owned())
}

pub(crate) fn tokens(references: Option<String>) -> Vec<String> {
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
    .filter_map(|o| o.hash)
    .collect())
}

pub async fn runtime_references_by_store_path_hash_for_trusted_content_variant<
    C: ConnectionTrait,
>(
    db: &C,
    reader: StorePathContentVariantReader,
    hashes: &[String],
) -> Result<HashMap<String, Vec<String>>, DbErr> {
    Ok(
        get_trusted_content_variant_for_each_store_path_hash_for_project_shared_build_or_binary_cache(
            db, reader, hashes,
        )
        .await?
        .into_iter()
        .map(|(hash, variant)| (hash, tokens(variant.references)))
        .collect(),
    )
}

fn runtime_closure_reachable_sql() -> String {
    format!(
        "{cte} \
         SELECT o.hash FROM derivation_output o JOIN runtime t ON t.derivation = o.derivation \
         WHERE o.hash IS NOT NULL \
         UNION SELECT u.h FROM unnest($1::text[]) AS u(h)",
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
    reader: StorePathContentVariantReader,
    seed_hashes: &[String],
) -> Result<HashMap<String, gradient_entity::cached_path::Model>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    if seed_hashes.is_empty() {
        return Ok(HashMap::new());
    }

    let walk = crate::graph::walks::begin_walk(db, &RUNTIME_CLOSURE_REACHABLE).await?;
    let reached_hashes: Vec<String> = RuntimeClosureStorePathHash::find_by_statement(
        RUNTIME_CLOSURE_REACHABLE.bind([seed_hashes.to_vec().into()]),
    )
    .all(&walk)
    .await?
    .into_iter()
    .map(|row| row.hash)
    .collect();
    walk.commit().await?;

    let reached =
        get_trusted_content_variant_for_each_store_path_hash_for_project_shared_build_or_binary_cache(
            db,
            reader,
            &reached_hashes,
        )
        .await?;
    Ok(referenced_from(seed_hashes, reached))
}

/// The derivation walk reaches every output of a reached derivation, the unreferenced ones too.
fn referenced_from(
    seed_hashes: &[String],
    mut reached: HashMap<String, gradient_entity::cached_path::Model>,
) -> HashMap<String, gradient_entity::cached_path::Model> {
    let mut queue: VecDeque<String> = seed_hashes.iter().cloned().collect();
    let mut closure = HashMap::new();
    while let Some(hash) = queue.pop_front() {
        let Some(path) = reached.remove(&hash) else {
            continue;
        };
        queue.extend(
            tokens(path.references.clone())
                .iter()
                .filter_map(|token| parse_reference_hash(token)),
        );
        closure.insert(hash, path);
    }

    closure
}

pub async fn runtime_closure_size<C>(
    db: &C,
    reader: StorePathContentVariantReader,
    seed_hashes: &[String],
) -> Result<i64, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let reached = runtime_closure_reachable(db, reader, seed_hashes).await?;
    Ok(reached.values().map(|r| r.nar_size).sum())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{empty_db, mock_db, statements};

    #[test]
    fn reference_hash_strips_name() {
        assert_eq!(
            parse_reference_hash("0123456789abcdfghijklmnpqrsvwxyz-hello-2.10").as_deref(),
            Some("0123456789abcdfghijklmnpqrsvwxyz")
        );
        assert_eq!(parse_reference_hash("abc123-hello-2.10"), None);
        assert_eq!(parse_reference_hash("abc123"), None);
        assert_eq!(parse_reference_hash(""), None);
    }

    #[tokio::test]
    async fn empty_seeds_is_zero() {
        let db = empty_db();
        assert_eq!(
            runtime_closure_size(&db, project_reader_with_random_project_id(), &[])
                .await
                .unwrap(),
            0
        );
    }

    fn project_reader_with_random_project_id() -> StorePathContentVariantReader {
        StorePathContentVariantReader::Project(ProjectId::now_v7())
    }

    #[tokio::test]
    async fn runtime_closure_walk_over_real_rows_follows_runtime_and_both_kind_edges_only() {
        let path = |c: &str| format!("/nix/store/{}-closure.drv", c.repeat(32));
        let hash = |c: &str| c.repeat(32);
        let [root, build, runtime, both, below_runtime, below_build] =
            ["1", "2", "3", "4", "5", "6"];
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&path(below_runtime))
            .output(&hash(below_runtime))
            .derivation(&path(below_build))
            .output(&hash(below_build))
            .derivation(&path(build))
            .output(&hash(build))
            .runtime_depends_on(&path(below_build))
            .derivation(&path(runtime))
            .output(&hash(runtime))
            .runtime_depends_on(&path(below_runtime))
            .derivation(&path(both))
            .output(&hash(both))
            .derivation(&path(root))
            .output(&hash(root))
            .depends_on(&path(build))
            .runtime_depends_on(&path(runtime))
            .both_depends_on(&path(both))
            .into_database()
            .await;

        let reached: std::collections::BTreeSet<String> = db
            .connection()
            .query_all_raw(RUNTIME_CLOSURE_REACHABLE.bind([vec![hash(root)].into()]))
            .await
            .unwrap()
            .iter()
            .map(|row| row.try_get("", "hash").unwrap())
            .collect();
        db.finish().await;

        let expected = [root, runtime, both, below_runtime].map(hash);
        assert_eq!(reached, expected.into_iter().collect());
    }

    #[tokio::test]
    async fn the_reference_walk_runs_inside_the_raised_work_mem_transaction() {
        let db = mock_db()
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([Vec::<gradient_entity::cached_path::Model>::new()])
            .into_connection();
        assert!(
            runtime_closure_reachable(
                &db,
                project_reader_with_random_project_id(),
                &["abc".to_string()]
            )
            .await
            .expect("the walk runs")
            .is_empty()
        );

        let log = statements(db.into_transaction_log());
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

    fn path(hash: &str, references: &str) -> gradient_entity::cached_path::Model {
        gradient_entity::cached_path::Model {
            hash: hash.into(),
            nar_object_storage_key: hash.into(),
            package: format!("{hash}-pkg"),
            references: Some(references.into()),
            nar_size: 10,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn an_output_nothing_references_is_outside_the_runtime_closure() {
        let [root, lib, dev, headers] = ["1", "2", "3", "4"].map(|digit| digit.repeat(32));
        let reached_paths = vec![
            path(&root, &format!("{root}-pkg {lib}-pkg")),
            path(&lib, &format!("{lib}-pkg")),
            path(&dev, &format!("{dev}-pkg {lib}-pkg {headers}-pkg")),
            path(&headers, ""),
        ];
        let db = mock_db()
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([reached_paths.clone(), reached_paths])
            .into_connection();

        let reached = runtime_closure_reachable(
            &db,
            project_reader_with_random_project_id(),
            std::slice::from_ref(&root),
        )
        .await
        .expect("the walk runs");

        let mut hashes: Vec<&str> = reached.keys().map(String::as_str).collect();
        hashes.sort_unstable();
        assert_eq!(hashes, [root.as_str(), lib.as_str()]);
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
