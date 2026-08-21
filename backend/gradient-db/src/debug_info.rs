/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The DWARF build-id index behind `GET /cache/{cache}/debuginfo/{build_id}`.
//!
//! nix builds the same index when a binary cache is created with
//! `index-debug-info=true`: every `lib/debug/.build-id/<xx>/<yy>.debug` member of
//! an uploaded NAR becomes a lookup from build id to (NAR, member). We derive it
//! by walking the stored NAR of `separateDebugInfo` outputs - store paths whose
//! name ends in `-debug`, the only ones nixpkgs puts a build-id tree in - so the
//! scan touches a small slice of the cache instead of every upload.

use gradient_entity::cached_path::{Entity as ECachedPath, Model as MCachedPath};
use gradient_entity::ids::{CacheId, CachedPathId};
use gradient_storage::NarStore;
use gradient_storage::debug_info::scan_build_ids;
use gradient_storage::nar_extract::nar_reader_from_stream;
use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, EntityTrait, Statement, Value};
use tracing::debug;

/// Store-path name suffix of a nixpkgs `separateDebugInfo` output.
const DEBUG_OUTPUT_SUFFIX: &str = "-debug";

/// The `cached_path_signature` join is the access gate: its row proves the
/// caller-authorised cache holds the path, the same rule the narinfo lookups use.
const LOOKUP_SQL: &str = "SELECT cp.file_hash AS file_hash, d.member AS member \
     FROM debug_info d \
     JOIN cached_path cp ON cp.id = d.cached_path \
     JOIN cached_path_signature s ON s.cached_path = cp.id AND s.cache = $1 \
     WHERE d.build_id = $2 AND cp.file_hash IS NOT NULL \
     ORDER BY d.created_at DESC \
     LIMIT 1";

/// Written out with the `-debug` pattern as a literal so the planner can match
/// it against the partial index covering exactly this predicate. A bound
/// parameter would not match, and the sweep would seq-scan `cached_path`.
const PENDING_SQL: &str = "SELECT * FROM cached_path \
     WHERE NOT debug_info_indexed AND package LIKE '%-debug' \
       AND file_hash IS NOT NULL \
     ORDER BY created_at \
     LIMIT $1";

/// A build id resolved inside one cache: which NAR holds it and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugInfoTarget {
    /// Compressed-NAR hash of the holding path, in `<algo>:<hash>` form.
    pub file_hash: String,
    /// NAR-relative path of the debug file.
    pub member: String,
}

/// Resolves `build_id` to a NAR this cache actually serves.
pub async fn lookup_for_cache<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
    build_id: &str,
) -> Result<Option<DebugInfoTarget>, DbErr> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            LOOKUP_SQL,
            [
                Value::Uuid(Some(Box::new(cache.into_inner()))),
                build_id.into(),
            ],
        ))
        .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    Ok(Some(DebugInfoTarget {
        file_hash: row.try_get("", "file_hash")?,
        member: row.try_get("", "member")?,
    }))
}

/// True when this store path is worth walking for a build-id tree.
pub fn carries_debug_info(package: &str) -> bool {
    package.ends_with(DEBUG_OUTPUT_SUFFIX)
}

/// The next batch of cached debug outputs whose NAR has not been walked yet.
/// Oldest first, so a backfill drains in ingest order.
pub async fn pending_debug_index<C: ConnectionTrait>(
    db: &C,
    limit: u64,
) -> Result<Vec<MCachedPath>, DbErr> {
    ECachedPath::find()
        .from_raw_sql(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            PENDING_SQL,
            [(limit as i64).into()],
        ))
        .all(db)
        .await
}

/// Walks one stored NAR and records its build ids, then marks the path scanned.
/// The marker is set even when the walk finds nothing - or when the object is
/// gone - so the same NAR is never read twice; a re-upload under a different
/// `file_hash` clears it again on ingest, which is also how a re-index is
/// forced. Returns the number of build ids recorded.
pub async fn index_cached_path<C: ConnectionTrait>(
    db: &C,
    nar_storage: &NarStore,
    cached_path: CachedPathId,
    hash: &str,
) -> anyhow::Result<usize> {
    if !needs_index(db, cached_path).await? {
        return Ok(0);
    }

    let entries = match nar_storage.get_stream(hash).await? {
        Some((_, stream)) => scan_build_ids(nar_reader_from_stream(stream)).await?,
        None => {
            debug!(%hash, "debug index: NAR object absent; marking scanned");
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

/// Re-read rather than trusted from the caller: a re-push of unchanged bytes
/// reaches the inline indexer with the marker already set, and decompressing a
/// whole debug output to rediscover the same members is pure waste.
async fn needs_index<C: ConnectionTrait>(db: &C, cached_path: CachedPathId) -> Result<bool, DbErr> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT debug_info_indexed FROM cached_path WHERE id = $1",
            [Value::Uuid(Some(Box::new(cached_path.into_inner())))],
        ))
        .await?;

    match row {
        Some(row) => Ok(!row.try_get::<bool>("", "debug_info_indexed")?),
        None => Ok(false),
    }
}

async fn insert_entries<C: ConnectionTrait>(
    db: &C,
    cached_path: CachedPathId,
    build_ids: Vec<String>,
    members: Vec<String>,
) -> Result<(), DbErr> {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO debug_info (id, build_id, cached_path, member, created_at) \
         SELECT uuidv7(), t.build_id, $1, t.member, $2 \
         FROM unnest($3::text[], $4::text[]) AS t(build_id, member) \
         ON CONFLICT (build_id, cached_path) DO NOTHING",
        [
            Value::Uuid(Some(Box::new(cached_path.into_inner()))),
            gradient_types::now().into(),
            build_ids.into(),
            members.into(),
        ],
    ))
    .await?;

    Ok(())
}

async fn mark_indexed<C: ConnectionTrait>(db: &C, cached_path: CachedPathId) -> Result<(), DbErr> {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE cached_path SET debug_info_indexed = true WHERE id = $1",
        [Value::Uuid(Some(Box::new(cached_path.into_inner())))],
    ))
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

    /// The sweep predicate must stay textually identical to the partial index in
    /// `m20260821_000000_debug_info`, pattern literal included - a bound
    /// parameter or a reordered clause silently turns every tick into a
    /// `cached_path` seq scan.
    #[test]
    fn the_backfill_predicate_matches_its_partial_index() {
        let sql = PENDING_SQL.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            sql.contains("WHERE NOT debug_info_indexed AND package LIKE '%-debug'"),
            "predicate must match the partial index verbatim: {sql}"
        );
        assert!(
            sql.contains("ORDER BY created_at"),
            "oldest first, on the index's ordering column: {sql}"
        );
    }

    /// A build id resolves only through a `cached_path_signature` row for the
    /// requesting cache. Dropping that join would serve one cache's debug info
    /// from another's URL.
    #[test]
    fn the_lookup_is_gated_on_this_cache_holding_the_path() {
        let sql = LOOKUP_SQL.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            sql.contains("JOIN cached_path_signature s ON s.cached_path = cp.id AND s.cache = $1"),
            "the cache gate must stay a join, not a filter that can be dropped: {sql}"
        );
        assert!(
            sql.contains("cp.file_hash IS NOT NULL"),
            "a path with no stored NAR has nothing to point at: {sql}"
        );
    }
}
