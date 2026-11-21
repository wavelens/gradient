/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::store_path_content_variant_reader_choice::cached_path_by_store_hash_subquery_sql;
use gradient_entity::cached_path::{Entity as ECachedPath, Model as MCachedPath};
use gradient_entity::ids::{CacheId, CachedPathId};
use gradient_storage::NarStore;
use gradient_storage::debug_info::scan_build_ids;
use gradient_storage::nar_extract::nar_reader_from_stream;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, Value};
use tracing::debug;

const DEBUG_OUTPUT_SUFFIX: &str = "-debug";

crate::sql_fn! {
    LOOKUP_SQL = || format!(
        "SELECT served.hash AS hash, d.member AS member \
         FROM debug_info d \
         JOIN cached_path cp ON cp.id = d.cached_path \
         CROSS JOIN LATERAL ({served}) served \
         WHERE d.build_id = $2 AND served.id = cp.id \
         ORDER BY d.created_at DESC \
         LIMIT 1",
        served = cached_path_by_store_hash_subquery_sql("cp.hash", "$1::uuid"),
    ),
        params = [CacheId, Text("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2")];
}

crate::sql! {
    /// The `-debug` pattern is a literal to match the partial index on this predicate.
    /// A bound parameter would not match, and the sweep would seq-scan `cached_path`.
    PENDING_SQL = "SELECT * FROM cached_path \
     WHERE NOT debug_info_indexed AND package LIKE '%-debug' \
     ORDER BY created_at \
     LIMIT $1",
        params = [Int(32)],
        tier = Sweep;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugInfoTarget {
    pub hash: String,
    pub member: String,
}

pub async fn lookup_for_cache<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    build_id: &str,
) -> Result<Option<DebugInfoTarget>, DbErr> {
    let row = db
        .query_one_raw(LOOKUP_SQL.bind([Value::Uuid(Some(cache.into_inner())), build_id.into()]))
        .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    Ok(Some(DebugInfoTarget {
        hash: row.try_get("", "hash")?,
        member: row.try_get("", "member")?,
    }))
}

pub fn carries_debug_info(package: &str) -> bool {
    package.ends_with(DEBUG_OUTPUT_SUFFIX)
}

pub async fn pending_debug_index<C: ConnectionTrait>(
    db: &C,
    limit: u64,
) -> Result<Vec<MCachedPath>, DbErr> {
    ECachedPath::find()
        .from_raw_sql(PENDING_SQL.bind([(limit as i64).into()]))
        .all(db)
        .await
}

pub async fn index_cached_path<C: ConnectionTrait>(
    db: &C,
    nar_storage: &NarStore,
    cached_path: CachedPathId,
) -> anyhow::Result<usize> {
    let Some(key) = unindexed_nar_object_storage_key(db, cached_path).await? else {
        return Ok(0);
    };

    let entries = match nar_storage.get_stream(&key).await? {
        Some((_, stream)) => scan_build_ids(nar_reader_from_stream(stream)).await?,
        None => {
            debug!(%key, "debug index: NAR object absent; marking scanned");
            Vec::new()
        }
    };

    if !entries.is_empty() {
        let build_ids: Vec<String> = entries.iter().map(|e| e.build_id.clone()).collect();
        let members: Vec<String> = entries.iter().map(|e| e.member.clone()).collect();
        insert_entries(db, cached_path, build_ids, members).await?;
    }

    mark_indexed(db, cached_path).await?;
    Ok(entries.len())
}

crate::sql! {
    UNINDEXED_NAR_OBJECT_STORAGE_KEY_SELECT = "SELECT nar_object_storage_key FROM cached_path \
         WHERE id = $1 AND NOT debug_info_indexed",
        params = [CachedPathId];

    INSERT_DEBUG_INFO = "INSERT INTO debug_info (id, build_id, cached_path, member, created_at) \
         SELECT uuidv7(), t.build_id, $1, t.member, $2 \
         FROM unnest($3::text[], $4::text[]) AS t(build_id, member) \
         ON CONFLICT (build_id, cached_path) DO NOTHING",
        params = [CachedPathId, Now, CachedPathHashes(8), CachedPathHashes(8)];

    MARK_DEBUG_INFO_INDEXED = "UPDATE cached_path SET debug_info_indexed = true WHERE id = $1",
        params = [CachedPathId];
}

async fn unindexed_nar_object_storage_key<C: ConnectionTrait>(
    db: &C,
    cached_path: CachedPathId,
) -> Result<Option<String>, DbErr> {
    db.query_one_raw(
        UNINDEXED_NAR_OBJECT_STORAGE_KEY_SELECT.bind([Value::Uuid(Some(cached_path.into_inner()))]),
    )
    .await?
    .map(|row| row.try_get("", "nar_object_storage_key"))
    .transpose()
}

async fn insert_entries<C: ConnectionTrait>(
    db: &C,
    cached_path: CachedPathId,
    build_ids: Vec<String>,
    members: Vec<String>,
) -> Result<(), DbErr> {
    db.execute_raw(INSERT_DEBUG_INFO.bind([
        Value::Uuid(Some(cached_path.into_inner())),
        gradient_types::now().into(),
        build_ids.into(),
        members.into(),
    ]))
    .await?;

    Ok(())
}

async fn mark_indexed<C: ConnectionTrait>(db: &C, cached_path: CachedPathId) -> Result<(), DbErr> {
    db.execute_raw(MARK_DEBUG_INFO_INDEXED.bind([Value::Uuid(Some(cached_path.into_inner()))]))
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_separate_debug_info_outputs_are_scanned() {
        assert!(carries_debug_info("hello-2.12.1-debug"));
        assert!(!carries_debug_info("hello-2.12.1"));
        assert!(!carries_debug_info("debug-tool-1.0"));
    }

    #[test]
    fn the_backfill_predicate_matches_its_partial_index() {
        let sql = PENDING_SQL
            .text()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            sql.contains("WHERE NOT debug_info_indexed AND package LIKE '%-debug'"),
            "predicate must match the partial index verbatim: {sql}"
        );
        assert!(
            sql.contains("ORDER BY created_at"),
            "oldest first, on the index's ordering column: {sql}"
        );
    }

    #[tokio::test]
    async fn find_debug_info_by_build_id_finds_all_cache_signed_content_variants_even_when_producer_is_untrusted()
     {
        use gradient_fixtures::store_path_content_variants::{
            FIRST_KEY, UNTRUSTED, two_variants_of_one_hash_and_one_untrusted_hash,
        };

        let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
        readers
            .db
            .execute(&format!(
                "INSERT INTO debug_info (id, build_id, cached_path, member, created_at) \
                 SELECT uuidv7(), CASE WHEN hash = '{UNTRUSTED}' THEN 'untrusted-build-id' \
                                       ELSE 'trusted-build-id' END, id, 'lib/debug/member', now() \
                 FROM cached_path WHERE hash = '{UNTRUSTED}' OR nar_object_storage_key = '{FIRST_KEY}';"
            ))
            .await;
        let conn = readers.db.connection();

        let trusted = lookup_for_cache(&conn, readers.cache_one, "trusted-build-id")
            .await
            .expect("The trusted lookup ran.");
        let untrusted = lookup_for_cache(&conn, readers.cache_one, "untrusted-build-id")
            .await
            .expect("The untrusted lookup ran");

        assert!(
            trusted.is_some(),
            "Cache C1 served the variants that C1 trusted, so the lookup found the debug info."
        );
        assert_eq!(
            untrusted.map(|target| target.hash),
            Some(UNTRUSTED.to_owned()),
            "Cache C1 signed the only variant of this hash. The lookup found the debug info for that variant, even though no projects that subscribe trust the producer of that variant."
        );
        readers.db.finish().await;
    }
}
