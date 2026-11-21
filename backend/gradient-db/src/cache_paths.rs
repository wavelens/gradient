/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::store_path_content_variant_reader_choice::cached_path_by_store_hash_subquery_sql;
use gradient_entity::project_cache::{self, CacheSubscriptionMode};
use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, FromQueryResult, JoinType, QueryFilter,
    QuerySelect, RelationTrait, Select, Value,
};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub struct ServedPath {
    pub hash: String,
    pub package: String,
    pub nar_hash: String,
    pub nar_size: i64,
    pub nar_object_storage_key: String,
    pub deriver: Option<String>,
    pub ca: Option<String>,
    pub signatures: Vec<(String, Vec<u8>)>,
    pub referenced_store_paths: Vec<String>,
}

#[derive(FromQueryResult)]
struct ServedRow {
    id: CachedPathId,
    hash: String,
    package: String,
    nar_hash: String,
    nar_size: i64,
    nar_object_storage_key: String,
    deriver: Option<String>,
    ca: Option<String>,
    references: Option<String>,
    cache_name: String,
    signature: Vec<u8>,
}

#[derive(FromQueryResult)]
struct ServedHash {
    hash: String,
}

crate::sql_fn! {
    SERVED_PATH = || format!(
        "SELECT chosen.id, chosen.hash, chosen.package, chosen.nar_hash, chosen.nar_size, chosen.nar_object_storage_key, \
                chosen.deriver, chosen.ca, chosen.\"references\", c.name AS cache_name, s.signature \
         FROM cache c \
         CROSS JOIN LATERAL ({choice}) chosen \
         JOIN cached_path_signature s ON s.cached_path = chosen.id AND s.cache = c.id \
         WHERE c.id = ANY($2) \
         ORDER BY c.name",
        choice = cached_path_by_store_hash_subquery_sql("$1::text", "c.id"),
    ),
        params = [CachedPathHash, CacheIds(4)];

    SERVED_HASHES = || format!(
        "SELECT DISTINCT chosen.hash FROM unnest($1::text[]) wanted(hash) \
         JOIN cache c ON c.id = ANY($2) \
         CROSS JOIN LATERAL ({choice}) chosen",
        choice = cached_path_by_store_hash_subquery_sql("wanted.hash", "c.id"),
    ),
        params = [CachedPathHashes(64), CacheIds(4)];
}

fn cache_ids(caches: &[CacheId]) -> Value {
    caches
        .iter()
        .map(|c| c.into_inner())
        .collect::<Vec<uuid::Uuid>>()
        .into()
}

pub(crate) fn project_cache_read_subscription_rows_select() -> Select<EProjectCache> {
    EProjectCache::find()
        .join(JoinType::InnerJoin, project_cache::Relation::Cache.def())
        .filter(CProjectCache::Mode.ne(CacheSubscriptionMode::WriteOnly))
        .filter(CCache::Active.eq(true))
}

pub async fn project_read_caches<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<CacheId>, DbErr> {
    project_cache_read_subscription_rows_select()
        .select_only()
        .column(CProjectCache::Cache)
        .filter(CProjectCache::Project.eq(project))
        .into_tuple()
        .all(db)
        .await
}

pub async fn project_write_caches<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<CacheId>, DbErr> {
    let subscribed: Vec<CacheId> = EProjectCache::find()
        .filter(CProjectCache::Project.eq(project))
        .filter(CProjectCache::Mode.is_in([
            CacheSubscriptionMode::ReadWrite,
            CacheSubscriptionMode::WriteOnly,
        ]))
        .all(db)
        .await?
        .into_iter()
        .map(|link| link.cache)
        .collect();
    if subscribed.is_empty() {
        return Ok(Vec::new());
    }

    let mut active: Vec<CacheId> = ECache::find()
        .filter(CCache::Id.is_in(subscribed))
        .filter(CCache::Active.eq(true))
        .all(db)
        .await?
        .into_iter()
        .map(|cache| cache.id)
        .collect();
    active.sort();
    Ok(active)
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

    let variant = first.id;
    let mut path = ServedPath {
        signatures: vec![(first.cache_name, first.signature)],
        referenced_store_paths: crate::graph::runtime_closure::tokens(first.references),
        hash: first.hash,
        package: first.package,
        nar_hash: first.nar_hash,
        nar_size: first.nar_size,
        nar_object_storage_key: first.nar_object_storage_key,
        deriver: first.deriver,
        ca: first.ca,
    };
    path.signatures.extend(
        rows.filter(|r| r.id == variant)
            .map(|r| (r.cache_name, r.signature)),
    );

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
    use gradient_fixtures::mock::{empty_db, mock_db};
    use sea_orm::Value;
    use std::collections::BTreeMap;

    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn read_subscription_rows_select_matches_reading_subscription_sql_fragment() {
        let normalize_where_conditions = |sql: &str| -> HashSet<String> {
            sql.split_once(" WHERE ")
                .map_or("", |(_, conditions)| conditions)
                .replace('"', "")
                .split(" AND ")
                .map(|condition| {
                    let condition = condition.trim().trim_end_matches(" = TRUE");
                    condition
                        .split_once('.')
                        .map_or(condition, |(_, column)| column)
                        .to_owned()
                })
                .filter(|condition| !condition.starts_with("project = "))
                .collect()
        };
        let rows_select = sea_orm::QueryTrait::build(
            &project_cache_read_subscription_rows_select(),
            sea_orm::DbBackend::Postgres,
        )
        .to_string();

        assert_eq!(
            normalize_where_conditions(&rows_select),
            normalize_where_conditions(
                &crate::store_path_content_variant_trust::reading_subscription("project")
            ),
            "{rows_select}"
        );
    }

    fn row(cache_name: &str) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("id".to_owned(), Value::from(uuid::Uuid::nil())),
            (
                "references".to_owned(),
                Value::from(Some(format!("{HASH}-hello"))),
            ),
            ("hash".to_owned(), Value::from(HASH)),
            ("package".to_owned(), Value::from("hello")),
            ("nar_hash".to_owned(), Value::from("sha256:abc")),
            ("nar_size".to_owned(), Value::from(42_i64)),
            (
                "nar_object_storage_key".to_owned(),
                Value::from(format!("{HASH}-hello")),
            ),
            ("deriver".to_owned(), Value::from(None::<String>)),
            ("ca".to_owned(), Value::from(None::<String>)),
            ("cache_name".to_owned(), Value::from(cache_name)),
            ("signature".to_owned(), Value::from(vec![1_u8, 2, 3])),
        ])
    }

    #[tokio::test]
    async fn one_path_carries_the_signature_of_every_cache() {
        let db = mock_db()
            .append_query_results([vec![row("main"), row("mirror")]])
            .into_connection();

        let path = served_path(&db, &[CacheId::now_v7(), CacheId::now_v7()], HASH)
            .await
            .expect("query")
            .expect("served");
        assert_eq!(path.package, "hello");
        assert_eq!(path.nar_size, 42);
        let names: Vec<&str> = path.signatures.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["main", "mirror"]);
    }

    #[tokio::test]
    async fn a_path_without_rows_is_not_served() {
        let db = mock_db()
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let path = served_path(&db, &[CacheId::now_v7()], HASH)
            .await
            .expect("query");
        assert!(path.is_none());
    }

    #[tokio::test]
    async fn no_caches_serve_nothing_without_a_query() {
        let db = empty_db();

        assert!(served_path(&db, &[], HASH).await.expect("query").is_none());
        let hashes = served_hashes(&db, &[], &[HASH.to_owned()])
            .await
            .expect("query");
        assert!(hashes.is_empty());
        assert!(db.into_transaction_log().is_empty());
    }

    #[tokio::test]
    async fn served_path_returns_signatures_of_one_variant_per_hash() {
        use gradient_fixtures::store_path_content_variants::{
            FIRST_KEY, SHARED, two_variants_of_one_hash_and_one_untrusted_hash,
        };
        let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
        let (one, two) = (readers.cache_one, readers.cache_two);
        readers
            .db
            .execute("UPDATE cached_path SET \"references\" = nar_object_storage_key")
            .await;
        let conn = readers.db.connection();

        let alone = served_path(&conn, &[one], SHARED)
            .await
            .expect("query")
            .expect("served");
        assert_eq!(
            (
                alone.nar_hash.as_str(),
                alone.signatures.len(),
                alone.referenced_store_paths
            ),
            ("sha256:def", 1, vec![FIRST_KEY.to_owned()]),
            "Cache C1 can show only the variants that C1 can provide, with C1's own signature."
        );

        let together = served_path(&conn, &[one, two], SHARED)
            .await
            .expect("query")
            .expect("served");
        assert_eq!(
            together.signatures.len(),
            1,
            "A signature can belong only to the one variant with visible metadata when several caches load together."
        );

        let untrusted = served_hashes(
            &conn,
            &[one],
            &["33333333333333333333333333333333".to_owned()],
        )
        .await
        .expect("query");
        assert_eq!(
            untrusted.len(),
            1,
            "The cache can supply the hash of the only variant that it signed, even when zero subscribed projects trust a producer."
        );
        readers.db.finish().await;
    }
}
