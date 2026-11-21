/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;

use gradient_types::StorePath;
use gradient_wire::types::{CacheInfo, RequiredPath};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, FromQueryResult, TransactionTrait};

use crate::graph::walks::{begin_walk, runtime_closure_cte};
use crate::store_path_content_variant_trust::variant_rank;

#[derive(FromQueryResult)]
struct ClosurePath {
    hash: String,
    package: String,
    nar_size: Option<i64>,
    file_size: Option<i64>,
}

fn program_closure_sql() -> String {
    format!(
        "{cte} \
         SELECT o.hash, o.package, cp.nar_size, cp.file_size \
         FROM derivation_output o JOIN closure t ON t.derivation = o.derivation \
         LEFT JOIN LATERAL (SELECT c.nar_size, c.file_size FROM cached_path c \
              WHERE c.hash = o.hash ORDER BY {rank} LIMIT 1) cp ON true \
         WHERE o.hash IS NOT NULL AND o.package IS NOT NULL",
        rank = variant_rank("c", "NULL::uuid"),
        cte = runtime_closure_cte(
            "closure",
            "SELECT o.derivation FROM derivation_output o WHERE o.hash = ANY($1::text[])",
        ),
    )
}

crate::sql_fn! {
    EXEC_PROGRAM_CLOSURE = program_closure_sql,
        params = [CachedPathHashes(1)],
        tier = Walk,
        flags = [Walk];
}

pub async fn program_closure<C>(db: &C, roots: &[String]) -> Result<Vec<RequiredPath>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let hashes = store_path_hash_parts(roots.iter().map(String::as_str));
    if hashes.is_empty() {
        return Ok(roots.iter().map(|root| uncached(root.clone())).collect());
    }

    let walk = begin_walk(db, &EXEC_PROGRAM_CLOSURE).await?;
    let rows = ClosurePath::find_by_statement(EXEC_PROGRAM_CLOSURE.bind([hashes.into()]))
        .all(&walk)
        .await?;
    walk.commit().await?;
    Ok(required_paths(roots, rows))
}

pub(crate) async fn load_exec_job_program_closure_store_path_hashes_from_database<
    C: ConnectionTrait,
>(
    db: &C,
    program: &str,
) -> Result<HashSet<String>, DbErr> {
    let hashes = store_path_hash_parts([program]);
    if hashes.is_empty() {
        return Ok(HashSet::new());
    }

    let rows = ClosurePath::find_by_statement(EXEC_PROGRAM_CLOSURE.bind([hashes.into()]))
        .all(db)
        .await?;
    Ok(rows.into_iter().map(|row| row.hash).collect())
}

fn store_path_hash_parts<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    paths
        .into_iter()
        .filter_map(|path| path.strip_prefix("/nix/store/")?.split('/').next())
        .filter_map(|base| StorePath::parse_stored(base).ok())
        .map(|path| path.hash().to_owned())
        .collect()
}

fn required_paths(roots: &[String], rows: Vec<ClosurePath>) -> Vec<RequiredPath> {
    let mut seen = HashSet::new();
    let reached = rows.into_iter().map(|row| RequiredPath {
        path: StorePath::from_hash_and_name_without_validation(row.hash, row.package).full(),
        cache_info: row.nar_size.map(|nar_size| CacheInfo {
            nar_size: u64::try_from(nar_size).unwrap_or(0),
            file_size: row
                .file_size
                .and_then(|size| u64::try_from(size).ok())
                .unwrap_or(0),
        }),
    });
    reached
        .chain(roots.iter().cloned().map(uncached))
        .filter(|path| seen.insert(path.path.clone()))
        .collect()
}

fn uncached(path: String) -> RequiredPath {
    RequiredPath {
        path,
        cache_info: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::shape::shape;
    use gradient_types::{CacheId, EvaluationId};

    const TRAIN: &str = "1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a-train.drv";
    const TRAIN_OUT: &str = "2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b";
    const CUDA: &str = "3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c-cuda.drv";
    const CUDA_OUT: &str = "4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d";
    const PYTHON: &str = "5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f-python.drv";
    const PYTHON_OUT: &str = "6g6g6g6g6g6g6g6g6g6g6g6g6g6g6g6g";

    #[tokio::test]
    async fn test_program_closure_cache_size_includes_copies_in_caches_project_does_not_read() {
        let (reader, uploader) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let private_cache = CacheId::now_v7();
        let db = shape()
            .evaluation(reader)
            .derivation(TRAIN)
            .output(TRAIN_OUT)
            .evaluation(uploader)
            .into_cache(private_cache)
            .cached_path(TRAIN_OUT, "train")
            .into_database()
            .await;
        db.execute("DELETE FROM store_path_content_producer").await;
        db.execute(&format!(
            "INSERT INTO cached_path_signature (id, cached_path, cache, created_at) \
             SELECT uuidv7(), c.id, '{private_cache}', c.created_at \
             FROM cached_path c WHERE c.hash = '{TRAIN_OUT}'"
        ))
        .await;
        let root = format!("/nix/store/{TRAIN_OUT}-train");
        let nar_sizes: Vec<Option<u64>> = program_closure(&db.connection(), &[root])
            .await
            .expect("the closure reads")
            .into_iter()
            .map(|path| path.cache_info.map(|info| info.nar_size))
            .collect();
        db.finish().await;

        assert_eq!(
            nar_sizes,
            vec![Some(5)],
            "The size of this closure is the size of a copy that is only in another project's private cache."
        );
    }

    #[tokio::test]
    async fn the_program_closure_holds_every_runtime_dependency_with_its_size() {
        let evaluation = EvaluationId::now_v7();
        let (older_cache, newer_cache) = (CacheId::now_v7(), CacheId::now_v7());
        let db = shape()
            .evaluation(evaluation)
            .into_cache(older_cache)
            .into_cache(newer_cache)
            .derivation(CUDA)
            .output(CUDA_OUT)
            .derivation(PYTHON)
            .output(PYTHON_OUT)
            .cached_output(PYTHON)
            .derivation(TRAIN)
            .output(TRAIN_OUT)
            .runtime_depends_on(CUDA)
            .runtime_depends_on(PYTHON)
            .into_database()
            .await;
        db.execute(&format!(
            "UPDATE cached_path SET nar_size = 100 WHERE hash = '{PYTHON_OUT}'; \
             DELETE FROM store_path_content_producer; \
             INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
             SELECT uuidv7(), '{PYTHON_OUT}', '{PYTHON_OUT}-older', 'python', 'sha256:older-file', 'sha256:older', 70, \
                    min(created_at) - interval '1 day' FROM cached_path"
        ))
        .await;
        db.execute(&format!(
            "INSERT INTO cached_path_signature (id, cached_path, cache, created_at) \
             SELECT uuidv7(), c.id, \
                    (CASE WHEN c.nar_size = 70 THEN '{older_cache}' ELSE '{newer_cache}' END)::uuid, \
                    c.created_at \
             FROM cached_path c WHERE c.hash = '{PYTHON_OUT}'"
        ))
        .await;
        let root = format!("/nix/store/{TRAIN_OUT}-train");
        let mut closure = program_closure(&db.connection(), &[root])
            .await
            .expect("the closure reads");
        closure.sort_by(|a, b| a.path.cmp(&b.path));

        let sizes: Vec<(String, Option<u64>)> = closure
            .into_iter()
            .map(|path| {
                let hash = path.path["/nix/store/".len()..][..32].to_owned();
                (hash, path.cache_info.map(|info| info.nar_size))
            })
            .collect();
        assert_eq!(
            sizes,
            vec![
                (TRAIN_OUT.to_owned(), None),
                (CUDA_OUT.to_owned(), None),
                (PYTHON_OUT.to_owned(), Some(70)),
            ],
            "All paths appear once, with the size of their highest ranked content variant (the oldest one here)."
        );
        db.finish().await;
    }
}
