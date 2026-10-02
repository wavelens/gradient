/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::project_cache::{self, CacheSubscriptionMode};
use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, FromQueryResult, JoinType, QueryFilter,
    QuerySelect, RelationTrait, Value,
};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub struct ServedPath {
    pub hash: String,
    pub package: String,
    pub nar_hash: Option<String>,
    pub nar_size: Option<i64>,
    pub deriver: Option<String>,
    pub ca: Option<String>,
    pub signatures: Vec<(String, Vec<u8>)>,
}

#[derive(FromQueryResult)]
struct ServedRow {
    hash: String,
    package: String,
    nar_hash: Option<String>,
    nar_size: Option<i64>,
    deriver: Option<String>,
    ca: Option<String>,
    cache_name: String,
    signature: Vec<u8>,
}

#[derive(FromQueryResult)]
struct ServedHash {
    hash: String,
}

crate::sql! {
    SERVED_PATH = "SELECT cp.hash, cp.package, cp.nar_hash, cp.nar_size, cp.deriver, cp.ca, \
                   c.name AS cache_name, s.signature \
                   FROM cached_path cp \
                   JOIN cached_path_signature s ON s.cached_path = cp.id \
                   JOIN cache c ON c.id = s.cache \
                   WHERE cp.hash = $1 AND s.cache = ANY($2) \
                   AND s.signature IS NOT NULL AND cp.file_hash IS NOT NULL \
                   ORDER BY c.name",
        params = [CachedPathHash, CacheIds(4)];

    SERVED_HASHES = "SELECT DISTINCT cp.hash FROM cached_path cp \
                     JOIN cached_path_signature s ON s.cached_path = cp.id \
                     WHERE cp.hash = ANY($1) AND s.cache = ANY($2) \
                     AND s.signature IS NOT NULL AND cp.file_hash IS NOT NULL",
        params = [CachedPathHashes(64), CacheIds(4)];
}

fn cache_ids(caches: &[CacheId]) -> Value {
    caches
        .iter()
        .map(|c| c.into_inner())
        .collect::<Vec<uuid::Uuid>>()
        .into()
}

pub async fn project_read_caches<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<CacheId>, DbErr> {
    EProjectCache::find()
        .select_only()
        .column(CProjectCache::Cache)
        .join(JoinType::InnerJoin, project_cache::Relation::Cache.def())
        .filter(CProjectCache::Project.eq(project))
        .filter(CProjectCache::Mode.ne(CacheSubscriptionMode::WriteOnly))
        .filter(CCache::Active.eq(true))
        .into_tuple()
        .all(db)
        .await
}

pub async fn served_path<C: ConnectionTrait>(
    db: &C,
    caches: &[CacheId],
    hash: &str,
) -> Result<Option<ServedPath>, DbErr> {
    if caches.is_empty() {
        return Ok(None);
    }

    let rows = ServedRow::find_by_statement(SERVED_PATH.bind([hash.into(), cache_ids(caches)]))
        .all(db)
        .await?;

    let mut rows = rows.into_iter();
    let Some(first) = rows.next() else {
        return Ok(None);
    };

    let mut path = ServedPath {
        signatures: vec![(first.cache_name, first.signature)],
        hash: first.hash,
        package: first.package,
        nar_hash: first.nar_hash,
        nar_size: first.nar_size,
        deriver: first.deriver,
        ca: first.ca,
    };
    path.signatures
        .extend(rows.map(|r| (r.cache_name, r.signature)));

    Ok(Some(path))
}

pub async fn served_hashes<C: ConnectionTrait>(
    db: &C,
    caches: &[CacheId],
    hashes: &[String],
) -> Result<HashSet<String>, DbErr> {
    if caches.is_empty() || hashes.is_empty() {
        return Ok(HashSet::new());
    }

    let rows = crate::fetch_in_chunks(hashes, |chunk| async move {
        ServedHash::find_by_statement(SERVED_HASHES.bind([chunk.into(), cache_ids(caches)]))
            .all(db)
            .await
    })
    .await?;

    Ok(rows.into_iter().map(|r| r.hash).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;

    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn row(cache_name: &str) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("hash".to_owned(), Value::from(HASH)),
            ("package".to_owned(), Value::from("hello")),
            (
                "nar_hash".to_owned(),
                Value::from(Some("sha256:abc".to_owned())),
            ),
            ("nar_size".to_owned(), Value::from(Some(42_i64))),
            ("deriver".to_owned(), Value::from(None::<String>)),
            ("ca".to_owned(), Value::from(None::<String>)),
            ("cache_name".to_owned(), Value::from(cache_name)),
            ("signature".to_owned(), Value::from(vec![1_u8, 2, 3])),
        ])
    }

    #[tokio::test]
    async fn one_path_carries_the_signature_of_every_cache() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row("main"), row("mirror")]])
            .into_connection();

        let path = served_path(&db, &[CacheId::now_v7(), CacheId::now_v7()], HASH)
            .await
            .expect("query")
            .expect("served");
        assert_eq!(path.package, "hello");
        assert_eq!(path.nar_size, Some(42));
        let names: Vec<&str> = path.signatures.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["main", "mirror"]);
    }

    #[tokio::test]
    async fn a_path_without_rows_is_not_served() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let path = served_path(&db, &[CacheId::now_v7()], HASH)
            .await
            .expect("query");
        assert!(path.is_none());
    }

    #[tokio::test]
    async fn no_caches_serve_nothing_without_a_query() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        assert!(served_path(&db, &[], HASH).await.expect("query").is_none());
        let hashes = served_hashes(&db, &[], &[HASH.to_owned()])
            .await
            .expect("query");
        assert!(hashes.is_empty());
        assert!(db.into_transaction_log().is_empty());
    }
}
