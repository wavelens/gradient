/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Recording a stored NAR in the cache index.

use anyhow::Context as _;
use gradient_db::{DbContext, WorkerDb};
use gradient_entity::StorePath;
use gradient_sources::CacheSigner;
use gradient_types::ids::{CacheId, CachedPathId, CachedPathSignatureId};
use gradient_types::*;
use gradient_util::nix_hash::{is_nix32_hash, normalize_nar_hash};
use sea_orm::sea_query::{Expr, LockType, OnConflict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use tracing::{debug, trace, warn};

use crate::messages::{NarCommit, NarCommitted, SignTargets};

/// Record a stored NAR: the row, the runtime dependencies its references name, the need
/// those edges carry, the complete closure of the shared build seeded from them and the can-start side
/// of that flip.
///
/// Executes inside the graph writer's transaction, and only there. The pre-commit
/// presence endpoint is read under a row lock a pooled handle would release with
/// the statement that took it, which is a race against the maintenance retires with
/// no compile error and no runtime signal, so the handle is checked here instead.
/// The shared build locks are taken on that same transaction, always after the
/// `cached_path` one and never before.
pub(crate) async fn commit(ctx: &DbContext, c: &NarCommit) -> anyhow::Result<NarCommitted> {
    let db = &ctx.worker_db;
    let sp = StorePath::parse(&c.store_path).map_err(|e| anyhow::anyhow!("{e}"))?;
    if !is_nix32_hash(sp.hash()) {
        anyhow::bail!("malformed store path: {}", c.store_path);
    }

    let txn = db.as_transaction().context(
        "NarCommit must run inside a transaction: the complete-closure endpoint it decides from and the shared build locks it takes are only held under one",
    )?;

    let Upserted {
        cached_path,
        created,
        was_backed,
    } = upsert_cached_path(db, sp.hash(), sp.name(), c).await?;

    let producers =
        gradient_db::graph::reachability::producers_of_hashes(txn, &[sp.hash().to_owned()]).await?;

    // The NAR is where a built output's runtime references are learned, so the
    // graph edges they name are written from the same report the index is, and what
    // need those edges carry is updated from the shared build they hang off.
    let referenced =
        gradient_db::graph::runtime_dependencies::producers_of_tokens(txn, &c.references).await?;
    if !referenced.is_empty() {
        for producer in &producers {
            gradient_db::graph::runtime_dependencies::insert_runtime_dependencies(
                txn,
                *producer,
                &referenced,
            )
            .await?;
        }

        let settled =
            gradient_db::graph::can_start::update_and_settle_need(txn, &producers).await?;
        gradient_db::status::emit_transition_effects(ctx, &settled.changes).await;
    }

    // Complete closure is counted on the shared build, and the seed needs the endpoint this
    // commit destroyed: a path that had no NAR before is what makes its producers
    // present, and the row already says so by the time the seed reads it. A re-push
    // of a backed path changed no presence, so it is only recounted.
    let (freshly_present, recounted): (&[DerivationId], &[DerivationId]) = if was_backed {
        (&[], &producers)
    } else {
        (&producers, &[])
    };
    let seeded =
        gradient_db::graph::runtime_can_start::seed_runtime_deps(txn, freshly_present, recounted)
            .await?;
    let owners = if was_backed {
        Vec::new()
    } else {
        gradient_db::graph::reachability::derivations_with_hashes(txn, &[sp.hash().to_owned()])
            .await?
    };
    trace!(store_path = %c.store_path, complete = seeded.complete.len(), "shared build complete-closure ripple");
    advance_shared_builds(ctx, txn, &seeded.complete, &owners).await?;
    if !seeded.incomplete.is_empty() {
        warn!(store_path = %c.store_path, incomplete = seeded.incomplete.len(), "commit added incomplete references");
        retract_shared_builds(ctx, txn, &seeded.incomplete).await?;
    }

    let signed = sign_into_caches(ctx, &[(c, &sp, cached_path)])
        .await?
        .pop()
        .unwrap_or_default();
    let outputs_marked = EDerivationOutput::update_many()
        .col_expr(CDerivationOutput::IsCached, Expr::value(true))
        .col_expr(CDerivationOutput::CachedPath, Expr::value(cached_path))
        .filter(CDerivationOutput::Hash.eq(sp.hash()))
        .exec(db)
        .await
        .context("mark derivation outputs cached")?
        .rows_affected;
    trace!(store_path = %c.store_path, outputs_marked, created, "cached path committed");

    Ok(NarCommitted {
        cached_path,
        created,
        outputs_marked,
        signed,
    })
}

/// [`commit`] for a batch, one statement per step instead of a dozen per NAR: the
/// commit's cost is round trips, not the database's work. Any failure fails the
/// batch; the caller then commits its NARs one by one, so a bad one still fails
/// only its own uploader. Executes on the graph writer's transaction, like [`commit`].
pub(crate) async fn commit_batch(
    ctx: &DbContext,
    commits: &[NarCommit],
) -> anyhow::Result<Vec<NarCommitted>> {
    let db = &ctx.worker_db;
    let txn = db.as_transaction().context(
        "a NAR batch must run inside a transaction: the shared build locks it takes are only held under one",
    )?;
    let paths = commits
        .iter()
        .map(|c| {
            let sp = StorePath::parse(&c.store_path).map_err(|e| anyhow::anyhow!("{e}"))?;
            if !is_nix32_hash(sp.hash()) {
                anyhow::bail!("malformed store path: {}", c.store_path);
            }
            Ok(sp)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let hashes: Vec<String> = paths.iter().map(|sp| sp.hash().to_owned()).collect();
    if hashes
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
        != hashes.len()
    {
        anyhow::bail!("a NAR batch commits each path once");
    }

    let upserted = upsert_cached_paths(db, commits, &paths).await?;
    let (backed, fresh): (Vec<_>, Vec<_>) = hashes
        .iter()
        .zip(&upserted)
        .partition(|(_, u)| u.was_backed);
    let backed: Vec<String> = backed.into_iter().map(|(h, _)| h.clone()).collect();
    let fresh: Vec<String> = fresh.into_iter().map(|(h, _)| h.clone()).collect();

    commit_runtime_dependencies(ctx, txn, commits, &hashes).await?;

    let freshly_present =
        gradient_db::graph::reachability::producers_of_hashes(txn, &fresh).await?;
    let recounted = gradient_db::graph::reachability::producers_of_hashes(txn, &backed).await?;
    let seeded =
        gradient_db::graph::runtime_can_start::seed_runtime_deps(txn, &freshly_present, &recounted)
            .await?;
    let owners = gradient_db::graph::reachability::derivations_with_hashes(txn, &fresh).await?;
    advance_shared_builds(ctx, txn, &seeded.complete, &owners).await?;
    if !seeded.incomplete.is_empty() {
        warn!(
            nars = commits.len(),
            incomplete = seeded.incomplete.len(),
            "commit added incomplete references"
        );
        retract_shared_builds(ctx, txn, &seeded.incomplete).await?;
    }

    let signing: Vec<(&NarCommit, &StorePath, CachedPathId)> = commits
        .iter()
        .zip(&paths)
        .zip(&upserted)
        .map(|((c, sp), u)| (c, sp, u.cached_path))
        .collect();
    let signed = sign_into_caches(ctx, &signing).await?;

    let marked = mark_outputs_cached(db, &hashes).await?;
    Ok(hashes
        .iter()
        .zip(upserted)
        .zip(signed)
        .map(|((hash, u), signed)| NarCommitted {
            cached_path: u.cached_path,
            created: u.created,
            outputs_marked: marked.get(hash).copied().unwrap_or(0),
            signed,
        })
        .collect())
}

/// Every row of the batch under its `FOR NO KEY UPDATE` lock, taken in hash order, then
/// the existing ones refreshed and the new ones inserted together.
async fn upsert_cached_paths(
    db: &WorkerDb,
    commits: &[NarCommit],
    paths: &[StorePath],
) -> anyhow::Result<Vec<Upserted>> {
    let hashes: Vec<&str> = paths.iter().map(|sp| sp.hash()).collect();
    let mut existing: std::collections::HashMap<String, MCachedPath> = ECachedPath::find()
        .filter(CCachedPath::Hash.is_in(hashes))
        .order_by_asc(CCachedPath::Hash)
        .lock(LockType::NoKeyUpdate)
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.hash.clone(), row))
        .collect();

    let mut upserted = Vec::with_capacity(commits.len());
    let mut inserts = Vec::new();
    for (c, sp) in commits.iter().zip(paths) {
        match existing.remove(sp.hash()) {
            Some(row) => {
                upserted.push(Upserted {
                    cached_path: row.id,
                    created: false,
                    was_backed: row.is_fully_cached(),
                });
                refreshed(row, c).update(db).await?;
            }
            None => {
                let row = new_row(sp.hash(), sp.name(), c);
                upserted.push(Upserted {
                    cached_path: row.id,
                    created: true,
                    was_backed: false,
                });
                inserts.push(row.into_active_model());
            }
        }
    }
    if !inserts.is_empty() {
        ECachedPath::insert_many(inserts)
            .exec_without_returning(db)
            .await?;
    }

    Ok(upserted)
}

/// The runtime dependencies and need of the NARs a producer backs. A `.drv` or a
/// source has none, so one lookup settles the common batch; a batch that has any
/// takes them NAR by NAR, exactly as [`commit`] does.
async fn commit_runtime_dependencies(
    ctx: &DbContext,
    txn: &sea_orm::DatabaseTransaction,
    commits: &[NarCommit],
    hashes: &[String],
) -> anyhow::Result<()> {
    let referencing: Vec<(&NarCommit, &String)> = commits
        .iter()
        .zip(hashes)
        .filter(|(c, _)| !c.references.is_empty())
        .collect();
    let candidates: Vec<String> = referencing.iter().map(|(_, h)| (*h).clone()).collect();
    if candidates.is_empty()
        || gradient_db::graph::reachability::producers_of_hashes(txn, &candidates)
            .await?
            .is_empty()
    {
        return Ok(());
    }

    for (c, hash) in referencing {
        let producers =
            gradient_db::graph::reachability::producers_of_hashes(txn, std::slice::from_ref(hash))
                .await?;
        if producers.is_empty() {
            continue;
        }
        let referenced =
            gradient_db::graph::runtime_dependencies::producers_of_tokens(txn, &c.references)
                .await?;
        if referenced.is_empty() {
            continue;
        }
        for producer in &producers {
            gradient_db::graph::runtime_dependencies::insert_runtime_dependencies(
                txn,
                *producer,
                &referenced,
            )
            .await?;
        }
        let settled =
            gradient_db::graph::can_start::update_and_settle_need(txn, &producers).await?;
        gradient_db::status::emit_transition_effects(ctx, &settled.changes).await;
    }

    Ok(())
}

/// Back every output with its hash's row in one statement, counted per hash.
async fn mark_outputs_cached(
    db: &WorkerDb,
    hashes: &[String],
) -> anyhow::Result<std::collections::HashMap<String, u64>> {
    let rows = EDerivationOutput::update_many()
        .col_expr(CDerivationOutput::IsCached, Expr::value(true))
        .col_expr(
            CDerivationOutput::CachedPath,
            Expr::cust("(SELECT cp.id FROM cached_path cp WHERE cp.hash = derivation_output.hash)"),
        )
        .filter(CDerivationOutput::Hash.is_in(hashes.to_vec()))
        .exec_with_returning(db)
        .await
        .context("mark derivation outputs cached")?;
    let mut marked = std::collections::HashMap::new();
    for row in rows {
        *marked.entry(row.hash).or_insert(0) += 1;
    }
    Ok(marked)
}

/// The shared build side of a forward complete closure flip: the shared builds in `complete` can serve
/// their outputs, and the `owners` of a `.drv` this commit made present are
/// importable, so their gates may have opened.
///
/// One ordered lock over both sets, on the commit's own transaction: the mark is a
/// bound and not a claim, so a shared build whose predicate does not hold is passed over,
/// and `promote` executes under the same lock that produced its candidates.
async fn advance_shared_builds(
    ctx: &DbContext,
    txn: &sea_orm::DatabaseTransaction,
    complete: &[DerivationId],
    owners: &[DerivationId],
) -> anyhow::Result<()> {
    let lock =
        gradient_db::graph::can_start::lock_shared_builds(txn, &union(complete, owners)).await?;
    let mut changes = gradient_db::graph::can_start::became_fetchable(&lock).await?;
    changes.extend(gradient_db::graph::can_start::promote(txn, owners).await?);
    gradient_db::status::emit_transition_effects(ctx, &changes).await;

    Ok(())
}

/// The symmetric loss. A commit that reports a reference we do not have takes
/// shared builds OUT of complete closure, and one left fetchable against such a shared build
/// dispatches a build whose input nothing can provide - the forward-only half of
/// this pair is the dead zone this project has paid for repeatedly.
async fn retract_shared_builds(
    ctx: &DbContext,
    txn: &sea_orm::DatabaseTransaction,
    incomplete: &[DerivationId],
) -> anyhow::Result<()> {
    let lock = gradient_db::graph::can_start::lock_shared_builds(txn, incomplete).await?;
    let changes = gradient_db::graph::can_start::lost_fetchability(&lock).await?;
    gradient_db::status::emit_transition_effects(ctx, &changes).await;

    Ok(())
}

fn union(a: &[DerivationId], b: &[DerivationId]) -> Vec<DerivationId> {
    let mut all = a.to_vec();
    all.extend_from_slice(b);
    all.sort_unstable();
    all.dedup();

    all
}

/// The debug-info walk decompresses the whole NAR, so it is running detached.
pub(crate) fn after_commit(ctx: &DbContext, committed: &NarCommitted, store_path: &str) {
    let Ok(sp) = StorePath::parse(store_path) else {
        return;
    };
    if !gradient_db::caches::debug_info::carries_debug_info(sp.name()) {
        return;
    }

    let db = ctx.worker_db.detached();
    let nar_storage = ctx.storage.nar_storage.clone();
    let hash = sp.hash().to_owned();
    let cached_path = committed.cached_path;
    ctx.shutdown.spawn(async move {
        match gradient_db::caches::debug_info::index_cached_path(
            &db,
            &nar_storage,
            cached_path,
            &hash,
        )
        .await
        {
            Ok(0) => {}
            Ok(count) => debug!(%hash, count, "indexed debug-info build ids"),
            Err(e) => warn!(%hash, error = %e, "failed to index debug info"),
        }
    });
}

/// The row the commit wrote, and what it was before. `was_backed` is the
/// pre-commit endpoint of the presence flip, which no statement after this write
/// can recover: it is read under the row lock, so a maintenance retire rippling
/// the same counters from its own transaction cannot land between the read and the
/// write.
struct Upserted {
    cached_path: CachedPathId,
    created: bool,
    was_backed: bool,
}

/// Insert or refresh the row under its `FOR NO KEY UPDATE` lock. A duplicate-key error
/// on the insert propagates: the graph writer serialises commits, so there is no race to
/// recover from, and inside a transaction a re-select after a failed INSERT would
/// only replace the real error with 25P02.
async fn upsert_cached_path(
    db: &WorkerDb,
    hash: &str,
    package: &str,
    c: &NarCommit,
) -> anyhow::Result<Upserted> {
    match ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .lock(LockType::NoKeyUpdate)
        .one(db)
        .await?
    {
        Some(row) => {
            let id = row.id;
            let was_backed = row.is_fully_cached();
            refreshed(row, c).update(db).await?;
            Ok(Upserted {
                cached_path: id,
                created: false,
                was_backed,
            })
        }
        None => {
            let row = new_row(hash, package, c)
                .into_active_model()
                .insert(db)
                .await?;
            Ok(Upserted {
                cached_path: row.id,
                created: true,
                was_backed: false,
            })
        }
    }
}

/// An existing row brought up to the commit's report.
fn refreshed(row: MCachedPath, c: &NarCommit) -> ACachedPath {
    let file_hash = normalize_nar_hash(&c.file_hash);
    // Different bytes under the same store path: the recorded build-id
    // members no longer describe the NAR, so re-open it to the indexer.
    let rescan_debug_info = row.file_hash.as_deref() != Some(file_hash.as_str());
    let was_confirmed = row.confirmed;
    let mut active = row.into_active_model();
    active.file_size = Set(Some(c.file_size));
    active.file_hash = Set(Some(file_hash));
    if rescan_debug_info {
        active.debug_info_indexed = Set(false);
        active.confirmed = Set(c.confirmed);
    } else if c.confirmed && !was_confirmed {
        active.confirmed = Set(true);
    }

    active.nar_size = Set(Some(c.nar_size));
    active.nar_hash = Set(Some(normalize_nar_hash(&c.nar_hash)));
    active.references = Set(Some(c.references.join(" ")));
    if c.deriver.is_some() {
        active.deriver = Set(c.deriver.clone());
    }

    if c.ca.is_some() {
        active.ca = Set(c.ca.clone());
    }

    active
}

fn new_row(hash: &str, package: &str, c: &NarCommit) -> MCachedPath {
    MCachedPath {
        id: CachedPathId::now_v7(),
        hash: hash.to_owned(),
        package: package.to_owned(),
        file_hash: Some(normalize_nar_hash(&c.file_hash)),
        file_size: Some(c.file_size),
        nar_size: Some(c.nar_size),
        nar_hash: Some(normalize_nar_hash(&c.nar_hash)),
        deriver: c.deriver.clone(),
        ca: c.ca.clone(),
        references: Some(c.references.join(" ")),
        created_at: now(),
        confirmed: c.confirmed,
        ..Default::default()
    }
}

/// The `cached_path_signature` row of every target cache for each committed path,
/// signed where this server holds the cache's key, in one statement. A path every
/// producing task keeps private, or a cache whose key is missing, gets an unsigned
/// row for the sweep to fill. Returns the caches signed into, per commit.
async fn sign_into_caches(
    ctx: &DbContext,
    committed: &[(&NarCommit, &StorePath, CachedPathId)],
) -> anyhow::Result<Vec<Vec<CacheId>>> {
    let db = &ctx.worker_db;
    let mut signed = vec![Vec::new(); committed.len()];
    let claimed: Vec<usize> = (0..committed.len())
        .filter(|&i| !matches!(committed[i].0.targets, SignTargets::None))
        .collect();
    if claimed.is_empty() {
        return Ok(signed);
    }

    let hashes: Vec<String> = claimed
        .iter()
        .map(|&i| committed[i].1.hash().to_owned())
        .collect();
    let private = gradient_db::graph::reachability::private_output_hashes(db, &hashes).await?;
    let mut signers: std::collections::HashMap<SignTargets, Vec<(CacheId, Option<CacheSigner>)>> =
        Default::default();
    let mut rows = Vec::new();
    let ts = now();
    for i in claimed {
        let (c, sp, cached_path) = committed[i];
        if let std::collections::hash_map::Entry::Vacant(slot) = signers.entry(c.targets) {
            slot.insert(target_signers(ctx, c.targets).await?);
        }
        let fingerprint =
            (!private.contains(sp.hash())).then(|| (sp.full(), normalize_nar_hash(&c.nar_hash)));
        for (cache, signer) in &signers[&c.targets] {
            let signature = match (&fingerprint, signer) {
                (Some((store_path, nar_hash)), Some(signer)) => Some(signer.sign_narinfo_raw(
                    store_path,
                    nar_hash,
                    c.nar_size as u64,
                    &c.references,
                )),
                _ => None,
            };
            if signature.is_some() {
                signed[i].push(*cache);
            }
            rows.push(
                MCachedPathSignature {
                    id: CachedPathSignatureId::now_v7(),
                    cached_path,
                    cache: *cache,
                    signature,
                    created_at: ts,
                    ..Default::default()
                }
                .into_active_model(),
            );
        }
    }
    insert_signatures(db, rows).await;

    Ok(signed)
}

/// The caches `targets` names, each with a signer when its key decrypts.
async fn target_signers(
    ctx: &DbContext,
    targets: SignTargets,
) -> anyhow::Result<Vec<(CacheId, Option<CacheSigner>)>> {
    let db = &ctx.worker_db;
    let caches: Vec<MCache> = match targets {
        SignTargets::None => Vec::new(),
        SignTargets::Cache(id) => ECache::find_by_id(id).one(db).await?.into_iter().collect(),
        SignTargets::ProjectCaches(project) => {
            let ids: Vec<CacheId> = EProjectCache::find()
                .filter(CProjectCache::Project.eq(project))
                .all(db)
                .await?
                .into_iter()
                .map(|pc| pc.cache)
                .collect();
            ECache::find().filter(CCache::Id.is_in(ids)).all(db).await?
        }
    };

    Ok(caches
        .iter()
        .map(|cache| (cache.id, signer_for(ctx, cache)))
        .collect())
}

fn signer_for(ctx: &DbContext, cache: &MCache) -> Option<CacheSigner> {
    if cache.private_key.is_empty() {
        return None;
    }
    CacheSigner::from_cache(
        &ctx.config.secrets.crypt_file,
        cache,
        &ctx.config.server.serve_url,
    )
    .inspect_err(
        |e| warn!(cache = %cache.name, error = %e, "signer unavailable; the row stays unsigned"),
    )
    .ok()
}

/// A row already there keeps its signature and takes the new one only where it
/// had none, so a re-push never unsigns a path and a placeholder is filled.
async fn insert_signatures(db: &WorkerDb, rows: Vec<ACachedPathSignature>) {
    if rows.is_empty() {
        return;
    }

    let result = ECachedPathSignature::insert_many(rows)
        .on_conflict(
            OnConflict::columns([
                CCachedPathSignature::CachedPath,
                CCachedPathSignature::Cache,
            ])
            .value(
                CCachedPathSignature::Signature,
                Expr::cust("coalesce(cached_path_signature.signature, excluded.signature)"),
            )
            .to_owned(),
        )
        .exec(db)
        .await;
    if let Err(e) = result {
        warn!(error = %e, "insert cached_path_signature failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_ctx::ctx;
    use gradient_types::ids::ProjectId;
    use sea_orm::{
        DatabaseBackend, DatabaseConnection, MockDatabase, MockExecResult, Statement,
        TransactionTrait, Value,
    };
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use uuid::Uuid;

    const SP: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello-2.12";
    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    /// A parent of [`HASH`], for the ripple level a completing commit drives.
    const DEP_HASH: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn cache_id() -> CacheId {
        CacheId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000002").unwrap())
    }

    fn project() -> ProjectId {
        ProjectId::new(Uuid::parse_str("20000000-0000-0000-0000-000000000003").unwrap())
    }

    fn project_cache_row() -> gradient_entity::project_cache::Model {
        gradient_entity::project_cache::Model {
            project: project(),
            cache: cache_id(),
            ..Default::default()
        }
    }

    fn returned_cached_path(hash: &str) -> MCachedPath {
        MCachedPath {
            id: CachedPathId::new(Uuid::now_v7()),
            hash: hash.to_string(),
            package: "hello-2.12".to_string(),
            file_hash: Some("sha256:abc".to_string()),
            file_size: Some(5),
            nar_size: Some(5),
            nar_hash: Some("sha256:def".to_string()),
            created_at: now(),
            ..Default::default()
        }
    }

    fn commit_for(store_path: &str) -> NarCommit {
        NarCommit {
            store_path: store_path.to_owned(),
            file_hash: "sha256:abc".to_owned(),
            file_size: 5,
            nar_size: 5,
            nar_hash: "sha256:def".to_owned(),
            references: Vec::new(),
            deriver: None,
            ca: None,
            targets: SignTargets::None,
            confirmed: true,
        }
    }

    /// One row of a producer lookup, which projects the derivation alone.
    fn producer_row() -> BTreeMap<String, Value> {
        BTreeMap::from([("derivation".to_owned(), Value::from(Uuid::now_v7()))])
    }

    /// One region row of the bounded need walk: the shared build and what it now reads.
    fn need_row() -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("derivation".to_owned(), Value::from(Uuid::now_v7())),
            ("wanted".to_owned(), Value::from(true)),
        ])
    }

    fn exec(rows_affected: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected,
        }
    }

    fn statements(db: WorkerDb) -> Vec<String> {
        gradient_db::pool::statements(db.into_transaction_log())
    }

    /// The statements as sea-orm built them. The shared helper formats each with
    /// `{:?}`, which escapes the quotes sea-orm puts around every identifier, so
    /// an assertion on a generated statement reads the raw `sql` instead.
    fn raw_statements(db: WorkerDb) -> Vec<Statement> {
        gradient_db::pool::raw_statements(db.into_transaction_log())
    }

    /// The value a generated statement binds to `column`, through the placeholder
    /// the SET clause or the insert's column list gives it. Searching the value
    /// list for the bare `Bool(Some(false))` would match the `debug_info_indexed`
    /// these same statements write, and pass while `confirmed` went the other way.
    fn bound(stmt: &Statement, column: &str) -> Value {
        let quoted = format!("\"{column}\"");
        let position = match stmt.sql.split_once(&format!("{quoted} = $")) {
            Some((_, rest)) => rest
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<usize>()
                .expect("a placeholder number"),
            None => {
                let columns = stmt
                    .sql
                    .split_once('(')
                    .and_then(|(_, rest)| rest.split_once(')'))
                    .expect("an insert column list")
                    .0;
                columns
                    .split(", ")
                    .position(|c| c == quoted)
                    .unwrap_or_else(|| panic!("{column} is not written: {}", stmt.sql))
                    + 1
            }
        };

        stmt.values.as_ref().expect("the statement binds values").0[position - 1].clone()
    }

    /// `commit` executes in the graph writer's transaction and rejects a pooled handle,
    /// so every test drives one. The mock records the whole transaction as a single
    /// log entry whose synthetic `BEGIN`/`COMMIT` the shared helper drops, so the
    /// statement indices stay the ones the code issues.
    async fn commit_in_transaction(ctx: &DbContext, c: &NarCommit) -> anyhow::Result<NarCommitted> {
        let tx = Arc::new(ctx.worker_db.begin().await.expect("begin"));
        let scoped = ctx.in_transaction(Arc::clone(&tx));
        let committed = commit(&scoped, c).await;
        drop(scoped);
        Arc::try_unwrap(tx)
            .expect("no handle outlives the commit")
            .commit()
            .await
            .expect("commit the transaction");

        committed
    }

    /// The scripted commit, its context dropped so the pool handle the log is read
    /// from is the last one alive.
    async fn commit_and_log(db: DatabaseConnection, c: &NarCommit) -> Vec<String> {
        let (ctx, pool) = ctx(db).await;
        commit_in_transaction(&ctx, c).await.expect("commit");
        drop(ctx);

        statements(pool)
    }

    /// [`commit_and_log`] over the raw statements.
    async fn commit_and_raw_log(db: DatabaseConnection, c: &NarCommit) -> Vec<Statement> {
        let (ctx, pool) = ctx(db).await;
        commit_in_transaction(&ctx, c).await.expect("commit");
        drop(ctx);

        raw_statements(pool)
    }

    /// An existing backed row re-pushed with `references`: nothing is freshly
    /// present, and the producer lookups answer with none, so no edge and no
    /// counter move follows the write.
    async fn recommit_log(references: Vec<String>) -> Vec<String> {
        let mut mock = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![returned_cached_path(HASH)],
                vec![returned_cached_path(HASH)],
            ])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()]);
        if !references.is_empty() {
            mock = mock.append_query_results([Vec::<BTreeMap<String, Value>>::new()]);
        }

        let db = mock.append_exec_results([exec(1)]).into_connection();

        commit_and_log(
            db,
            &NarCommit {
                references,
                ..commit_for(SP)
            },
        )
        .await
    }

    fn secret_file() -> (tempfile::NamedTempFile, String) {
        use std::io::Write as _;

        let mut file = tempfile::NamedTempFile::new().expect("temp secret");
        file.write_all(b"test-secret-key-32-bytes-padding!")
            .expect("write secret");
        let path = file.path().to_string_lossy().to_string();
        (file, path)
    }

    fn cache_row(private_key: String) -> MCache {
        MCache {
            id: cache_id(),
            name: "main".into(),
            display_name: "Main".into(),
            description: String::new(),
            active: true,
            priority: 0,
            local_priority: None,
            public_key: String::new(),
            private_key,
            public: true,
            created_by: gradient_types::ids::UserId::now_v7(),
            created_at: now(),
            managed: false,
            max_storage_gb: 0,
        }
    }

    /// The scripted answers of a commit that resolves its project to one cache.
    fn project_commit_db(cache: MCache) -> DatabaseConnection {
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![returned_cached_path(HASH)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![project_cache_row()]])
            .append_query_results([vec![cache]])
            .append_exec_results([exec(1), exec(1)])
            .into_connection()
    }

    /// A resolved project writes a `cached_path_signature` row for every
    /// subscribed cache, signed with the cache's key. Regression guard: the
    /// detached NAR commit must resolve the project on the read loop before the
    /// job is evicted from the tracker, otherwise `SignTargets` collapses to
    /// `None`, no row is written, and the narinfo 404s forever.
    #[tokio::test]
    async fn a_project_target_signs_with_the_caches_key() {
        let (_file, secret) = secret_file();
        let (private_key, _) =
            gradient_sources::generate_signing_key(&secret).expect("signing key");
        let (ctx, pool) = crate::test_ctx::ctx_with_crypt_file(
            project_commit_db(cache_row(private_key)),
            &secret,
        )
        .await;

        let committed = commit_in_transaction(
            &ctx,
            &NarCommit {
                targets: SignTargets::ProjectCaches(project()),
                ..commit_for(SP)
            },
        )
        .await
        .expect("commit");
        drop(ctx);

        assert_eq!(committed.signed, vec![cache_id()]);
        let log = raw_statements(pool);
        let insert = log
            .iter()
            .find(|s| s.sql.contains("INSERT INTO \"cached_path_signature\""))
            .expect("the signature row");
        assert!(
            matches!(bound(insert, "signature"), Value::Bytes(Some(sig)) if sig.len() == 64),
            "{insert:?}"
        );
    }

    /// A cache whose key this server does not hold still gets its row, unsigned,
    /// for the sweep: "not yet signed" stays distinct from "never signed".
    #[tokio::test]
    async fn a_cache_without_a_key_gets_an_unsigned_row() {
        let (ctx, pool) = ctx(project_commit_db(cache_row(String::new()))).await;

        let committed = commit_in_transaction(
            &ctx,
            &NarCommit {
                targets: SignTargets::ProjectCaches(project()),
                ..commit_for(SP)
            },
        )
        .await
        .expect("commit");
        drop(ctx);

        assert!(committed.signed.is_empty());
        let log = raw_statements(pool);
        let insert = log
            .iter()
            .find(|s| s.sql.contains("INSERT INTO \"cached_path_signature\""))
            .expect("the signature row");
        assert_eq!(bound(insert, "signature"), Value::Bytes(None), "{insert:?}");
    }

    #[tokio::test]
    async fn recording_keeps_the_content_address() {
        let ca = "text:sha256:006vc8gixyrcynsx4lz1qxingl0mdja3l0xw1nl0j73isg37x944";
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![returned_cached_path(HASH)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results([exec(0)])
            .into_connection();

        let log = commit_and_log(
            db,
            &NarCommit {
                ca: Some(ca.to_owned()),
                ..commit_for(SP)
            },
        )
        .await;

        assert!(
            log.iter().any(|s| s.contains(ca)),
            "the content address must be written to cached_path"
        );
    }

    /// No resolvable project records the path but enqueues no signature, so the
    /// endpoint can distinguish "not yet signed" from "will never be signed".
    #[tokio::test]
    async fn none_target_enqueues_no_signature() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![returned_cached_path(HASH)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results([exec(0)])
            .into_connection();

        let log = commit_and_log(db, &commit_for(SP)).await;

        assert!(
            !log.iter().any(|s| s.contains("cached_path_signature")),
            "None target must not touch cached_path_signature"
        );
    }

    /// The NAR is where a built output's runtime references are learned: every
    /// reference with a producer becomes a runtime dependency from the path's producer,
    /// and the need those edges carry is updated over the producer at once.
    /// Without it the shared builds the new edges reach wait a sweep interval for need
    /// they already have, which is the whole point of learning them here.
    #[tokio::test]
    async fn a_commit_writes_the_runtime_dependencies_its_references_name() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![returned_cached_path(HASH)]])
            .append_query_results([vec![producer_row()]])
            .append_query_results([vec![producer_row()]])
            .append_query_results([vec![need_row()]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results([exec(1), exec(0), exec(1), exec(1), exec(0)])
            .into_connection();

        let log = commit_and_log(
            db,
            &NarCommit {
                references: vec![format!("{DEP_HASH}-dep")],
                ..commit_for(SP)
            },
        )
        .await;

        let edges = log
            .iter()
            .position(|s| {
                s.contains("INSERT INTO derivation_dependency (derivation, dependency, kind)")
            })
            .expect("the runtime dependencies are written");
        let need = log
            .iter()
            .position(|s| s.contains("region(evaluation, derivation, builder) AS"))
            .expect("need is updated over the producer");
        assert!(
            edges < need,
            "the update must see the edges it walks: {log:?}"
        );
    }

    /// A NAR that makes its producer present advances the shared build side in the SAME
    /// transaction: the producer's counter is seeded, what became complete is offered
    /// to the fetchable mark, and the derivation whose own `.drv` this is is offered
    /// to promotion. Without this the counters only move on the next sweep, and a
    /// parent waits a sweep interval for an input it already has.
    #[tokio::test]
    async fn a_complete_commit_advances_the_shared_builds_behind_the_paths_it_completed() {
        let producer = Uuid::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![returned_cached_path(HASH)]])
            .append_query_results([vec![BTreeMap::from([(
                "derivation".to_owned(),
                Value::from(producer),
            )])]])
            .append_query_results([vec![BTreeMap::from([
                ("derivation".to_owned(), Value::from(producer)),
                ("was_complete".to_owned(), Value::from(false)),
                ("complete".to_owned(), Value::from(true)),
            ])]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![BTreeMap::from([(
                "id".to_owned(),
                Value::from(Uuid::now_v7()),
            )])]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results([exec(1), exec(1), exec(0)])
            .into_connection();

        let log = commit_and_log(db, &commit_for(SP)).await;

        let seed = log
            .iter()
            .position(|s| s.contains("SET missing_runtime_deps = x.n"))
            .expect("the producer's counter is seeded");
        let mark = log
            .iter()
            .position(|s| s.contains("SET fetchable = true"))
            .expect("what became complete is offered to the mark");
        let promote = log
            .iter()
            .position(|s| s.contains("SET status = 1"))
            .expect("the owner of the .drv is offered to promotion");
        assert!(
            seed < mark && mark < promote,
            "the shared build side follows the seed that produced its set: {log:?}"
        );
    }

    /// The pre-commit presence endpoint must be read under the row lock. A
    /// maintenance retire deletes the same row from its own transaction, outside
    /// the graph writer, so an unlocked read lets the delete land between the read
    /// and the write: the commit would then call its producers freshly present
    /// against a row that is gone and count every parent down a second time,
    /// permanently. The row read is the commit's first statement.
    #[tokio::test]
    async fn the_pre_commit_endpoint_is_read_under_the_row_lock() {
        let log = recommit_log(Vec::new()).await;
        assert!(
            log[0].contains("FOR NO KEY UPDATE"),
            "the row read that holds the endpoint must lock it: {log:?}"
        );
    }

    /// The narinfo `References:` line is one ordered text column on the path now,
    /// written from the same report the runtime dependencies are, so the line and the
    /// signature fingerprint over it reconstruct verbatim.
    #[tokio::test]
    async fn a_commit_writes_the_reported_references_onto_the_row() {
        let dep = format!("{DEP_HASH}-dep");
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![returned_cached_path(HASH)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results([exec(0)])
            .into_connection();

        let log = commit_and_raw_log(
            db,
            &NarCommit {
                references: vec![dep.clone(), format!("{HASH}-hello-2.12")],
                ..commit_for(SP)
            },
        )
        .await;

        let insert = log
            .iter()
            .find(|s| s.sql.starts_with("INSERT INTO \"cached_path\""))
            .expect("the insert");
        assert_eq!(
            bound(insert, "references"),
            Value::from(format!("{dep} {HASH}-hello-2.12")),
            "{insert:?}"
        );
    }

    /// A pooled handle releases every lock at the end of the statement that took
    /// it, so a commit on one races the maintenance retires with nothing held.
    /// Neither the compiler nor a `MockDatabase` can tell the two handles apart,
    /// so the commit refuses the pooled one instead of silently racing.
    #[tokio::test]
    async fn a_pooled_commit_is_rejected_instead_of_racing_the_retires() {
        let (pooled, _pool) =
            ctx(MockDatabase::new(DatabaseBackend::Postgres).into_connection()).await;

        let err = commit(&pooled, &commit_for(SP))
            .await
            .expect_err("a pooled commit must not run");

        assert!(
            err.to_string().contains("must run inside a transaction"),
            "{err}"
        );
    }

    /// A commit that names a reference whose producer is not complete takes its OWN
    /// producer out of complete closure: the new runtime dependency is a missing dependency, the seed reports
    /// the loss, and every shared build left fetchable against it dispatches a build whose
    /// input nothing can provide. The forward-only half of this pair is the dead
    /// zone this project has paid for repeatedly.
    #[tokio::test]
    async fn a_commit_that_loses_completeness_ripples_backward() {
        let producer = Uuid::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![returned_cached_path(HASH)],
                vec![returned_cached_path(HASH)],
            ])
            .append_query_results([vec![BTreeMap::from([(
                "derivation".to_owned(),
                Value::from(producer),
            )])]])
            .append_query_results([vec![producer_row()]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![BTreeMap::from([
                ("derivation".to_owned(), Value::from(producer)),
                ("was_complete".to_owned(), Value::from(true)),
                ("complete".to_owned(), Value::from(false)),
            ])]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results([exec(1), exec(0), exec(1), exec(1), exec(1), exec(1)])
            .into_connection();

        let log = commit_and_log(
            db,
            &NarCommit {
                references: vec!["cccccccccccccccccccccccccccccccc-gone".to_owned()],
                ..commit_for(SP)
            },
        )
        .await;

        assert!(
            log.iter()
                .any(|s| s
                    .contains("INSERT INTO derivation_dependency (derivation, dependency, kind)")),
            "the missing dependency is recorded as a runtime dependency: {log:?}"
        );
        assert!(
            log.iter().any(|s| s.contains("SET fetchable = false")),
            "a shared build that stopped being complete must stop being fetchable: {log:?}"
        );
    }

    #[tokio::test]
    async fn a_committed_path_backs_every_output_with_its_hash() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![returned_cached_path(HASH)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results([exec(2)])
            .into_connection();
        let (ctx, _pool) = ctx(db).await;

        let committed = commit_in_transaction(&ctx, &commit_for(SP)).await.unwrap();
        assert!(committed.created);
        assert_eq!(committed.outputs_marked, 2);
    }

    /// A passthrough NAR on S3 is committed before its object exists, so the row
    /// must say so.
    #[tokio::test]
    async fn a_passed_through_commit_on_s3_inserts_the_row_unconfirmed() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![returned_cached_path(HASH)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results([exec(0)])
            .into_connection();

        let log = commit_and_raw_log(
            db,
            &NarCommit {
                confirmed: false,
                ..commit_for(SP)
            },
        )
        .await;

        let insert = log
            .iter()
            .find(|s| s.sql.starts_with("INSERT INTO \"cached_path\""))
            .expect("the insert");
        assert_eq!(
            bound(insert, "confirmed"),
            Value::Bool(Some(false)),
            "{insert:?}"
        );
    }

    /// New bytes under an old hash on S3 are unconfirmed again until uploaded.
    #[tokio::test]
    async fn a_recommit_with_new_bytes_takes_the_commits_confirmed_flag() {
        let existing = MCachedPath {
            confirmed: true,
            file_hash: Some("sha256:old".to_owned()),
            ..returned_cached_path(HASH)
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![existing.clone()], vec![existing]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results([exec(1)])
            .into_connection();

        let log = commit_and_raw_log(
            db,
            &NarCommit {
                confirmed: false,
                ..commit_for(SP)
            },
        )
        .await;

        let update = log
            .iter()
            .find(|s| s.sql.starts_with("UPDATE \"cached_path\""))
            .expect("the update");
        assert_eq!(
            bound(update, "confirmed"),
            Value::Bool(Some(false)),
            "{update:?}"
        );
    }
}
