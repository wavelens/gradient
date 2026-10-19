/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Context as _;
use gradient_db::{DbContext, WorkerDb};
use gradient_entity::{StorePath, StorePathError};
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

/// The commit must run inside the graph writer's transaction only.
/// A pooled handle would release the presence row lock with its statement and race the retires.
/// The shared build locks are always taken after the `cached_path` lock, never before.
pub(crate) async fn commit(ctx: &DbContext, c: &NarCommit) -> anyhow::Result<NarCommitted> {
    let db = &ctx.worker_db;
    let sp = StorePath::parse_stored(&c.store_path)?;
    if !is_nix32_hash(sp.hash()) {
        anyhow::bail!(StorePathError::Malformed(c.store_path.clone()));
    }

    let txn = db.as_transaction().context(
        "NarCommit must run inside a transaction: the complete-closure endpoint it decides from and the shared build locks it takes are only held under one",
    )?;

    let Upserted {
        cached_path,
        created,
        was_backed,
        recorded,
    } = upsert_cached_path(db, sp.hash(), sp.name(), c).await?;
    let c = &recorded;

    let producers =
        gradient_db::graph::reachability::producers_of_hashes(txn, &[sp.hash().to_owned()]).await?;

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
        gradient_db::status::emit_transition_effects(ctx, &settled.changes).await?;
    }

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
    trace!(store_path = %c.store_path, created, "cached path committed");

    Ok(NarCommitted {
        cached_path,
        created,
        signed,
    })
}

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
            let sp = StorePath::parse_stored(&c.store_path)?;
            if !is_nix32_hash(sp.hash()) {
                anyhow::bail!(StorePathError::Malformed(c.store_path.clone()));
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
    let recorded: Vec<NarCommit> = upserted.iter().map(|u| u.recorded.clone()).collect();
    let commits = recorded.as_slice();
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

    Ok(upserted
        .into_iter()
        .zip(signed)
        .map(|(u, signed)| NarCommitted {
            cached_path: u.cached_path,
            created: u.created,
            signed,
        })
        .collect())
}

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
                let (existing, update) = existing_row(row, c);
                upserted.push(existing);
                if let Some(update) = update {
                    update.update(db).await?;
                }
            }
            None => {
                let row = new_row(sp.hash(), sp.name(), c);
                upserted.push(Upserted {
                    cached_path: row.id,
                    created: true,
                    was_backed: false,
                    recorded: c.clone(),
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
        gradient_db::status::emit_transition_effects(ctx, &settled.changes).await?;
    }

    Ok(())
}

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
    gradient_db::status::emit_transition_effects(ctx, &changes).await?;

    Ok(())
}

/// A commit reporting a reference we lack is taking shared builds out of complete closure.
/// A shared build left fetchable against one would dispatch a build with an unprovidable input.
async fn retract_shared_builds(
    ctx: &DbContext,
    txn: &sea_orm::DatabaseTransaction,
    incomplete: &[DerivationId],
) -> anyhow::Result<()> {
    let lock = gradient_db::graph::can_start::lock_shared_builds(txn, incomplete).await?;
    let changes = gradient_db::graph::can_start::lost_fetchability(&lock).await?;
    gradient_db::status::emit_transition_effects(ctx, &changes).await?;

    Ok(())
}

fn union(a: &[DerivationId], b: &[DerivationId]) -> Vec<DerivationId> {
    let mut all = a.to_vec();
    all.extend_from_slice(b);
    all.sort_unstable();
    all.dedup();

    all
}

pub(crate) fn after_commit(ctx: &DbContext, committed: &NarCommitted, store_path: &str) {
    let Ok(sp) = StorePath::parse_stored(store_path) else {
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

struct Upserted {
    cached_path: CachedPathId,
    created: bool,
    was_backed: bool,
    recorded: NarCommit,
}

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
            let (existing, update) = existing_row(row, c);
            if let Some(update) = update {
                update.update(db).await?;
            }

            Ok(existing)
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
                recorded: c.clone(),
            })
        }
    }
}

fn existing_row(row: MCachedPath, c: &NarCommit) -> (Upserted, Option<ACachedPath>) {
    let mut existing = Upserted {
        cached_path: row.id,
        created: false,
        was_backed: row.is_fully_cached(),
        recorded: c.clone(),
    };
    if !row.is_stored() {
        return (existing, Some(refreshed(row, c)));
    }

    existing.recorded = recorded_by_stored_row(&row, c);
    let update = (c.deriver.is_some() && c.deriver != row.deriver).then(|| ACachedPath {
        deriver: Set(c.deriver.clone()),
        ..row.into_active_model()
    });

    (existing, update)
}

fn recorded_by_stored_row(row: &MCachedPath, c: &NarCommit) -> NarCommit {
    let same = |stored: &Option<String>, offered: &str| {
        stored.as_deref().map(normalize_nar_hash) == Some(normalize_nar_hash(offered))
    };
    let same_content = same(&row.file_hash, &c.file_hash) && same(&row.nar_hash, &c.nar_hash);
    if !same_content {
        debug!(store_path = %c.store_path, stored = ?row.nar_hash, offered = %c.nar_hash, "kept the first stored content");
    }

    let targets = if same_content || c.built_by_worker {
        c.targets
    } else {
        SignTargets::None
    };

    NarCommit {
        deriver: c.deriver.clone().or_else(|| row.deriver.clone()),
        built_by_worker: c.built_by_worker,
        ..NarCommit::from_stored_row(row, targets)
    }
}

fn refreshed(row: MCachedPath, c: &NarCommit) -> ACachedPath {
    let file_hash = normalize_nar_hash(&c.file_hash);
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
                (Some((store_path, nar_hash)), Some(signer)) => Some(signer.sign_stored_narinfo(
                    &ctx.store_path_mapper(MappingContext::Cache(*cache)),
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

pub(crate) fn signer_for(ctx: &DbContext, cache: &MCache) -> Option<CacheSigner> {
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

pub(crate) async fn insert_signatures(db: &WorkerDb, rows: Vec<ACachedPathSignature>) {
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
