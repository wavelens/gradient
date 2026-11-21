/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Context as _;
use gradient_db::store_path_content_variant_trust::{
    AllowedCache, caches_allowed_to_sign_committed_store_path_content_variant,
    get_project_build_output_cache_ids, get_project_sign_output_cache_ids,
};
use gradient_db::{DbContext, WorkerDb};
use gradient_entity::cached_path::join_sorted_deduplicated_references_with_spaces;
use gradient_entity::store_path_content_producer::{self, StoredContentVariantProducerKind};
use gradient_entity::{StorePath, StorePathError};
use gradient_sources::CacheSigner;
use gradient_storage::ServerComputedNarAndCompressedFileHashesAndSizes;
use gradient_types::ids::{
    CacheId, CachedPathId, CachedPathSignatureId, StorePathContentProducerId,
};
use gradient_types::*;
use gradient_util::nix_hash::is_nix32_hash;
use sea_orm::sea_query::{Alias, Expr, ExprTrait, LockType, OnConflict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect,
};
use std::collections::{HashMap, HashSet};
use tracing::{debug, trace, warn};

use crate::messages::{NarCommit, NarCommitContent, NarCommitted, StoredContentProducer};

/// The commit must run inside the graph writer's transaction only.
/// A pooled handle would release the presence row lock with its statement and race the retires.
/// The shared build locks are always taken after the `cached_path` lock, never before.
pub(crate) async fn commit(
    ctx: &DbContext,
    c: &NarCommit,
    existing_object_keys: &HashSet<String>,
) -> anyhow::Result<NarCommitted> {
    let db = &ctx.worker_db;
    let sp = StorePath::parse_stored(&c.store_path)?;
    if !is_nix32_hash(sp.hash()) {
        anyhow::bail!(StorePathError::Malformed(c.store_path.clone()));
    }

    let txn = db.as_transaction().context(
        "NarCommit must run inside a transaction: the complete-closure endpoint it decides from and the shared build locks it takes are only held under one",
    )?;

    let upserted = upsert_cached_path(db, sp.hash(), sp.name(), c, existing_object_keys).await?;
    let (created, was_backed) = (upserted.created, upserted.was_backed);
    let references = upserted.committed_store_path_references();

    let producers =
        gradient_db::graph::reachability::producers_of_hashes(txn, &[sp.hash().to_owned()]).await?;

    let referenced =
        gradient_db::graph::runtime_dependencies::producers_of_tokens(txn, &references).await?;
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

    let freshly_present = if was_backed {
        Vec::new()
    } else {
        gradient_db::graph::reachability::producers_of_hashes_with_output_not_in_shared_build_unneeded_outputs(
            txn,
            &[sp.hash().to_owned()],
        )
        .await?
    };
    let seeded =
        gradient_db::graph::runtime_can_start::seed_runtime_deps(txn, &freshly_present, &producers)
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

    let signed = sign_into_caches(ctx, &[(&c.producer, &sp, &upserted)])
        .await?
        .pop()
        .unwrap_or_default();
    ctx.request_signature_sweep_wake();
    trace!(store_path = %c.store_path, created, "cached path committed");

    Ok(NarCommitted {
        cached_path: upserted.row.id,
        nar_object_storage_key: upserted.row.nar_object_storage_key,
        created,
        signed,
    })
}

pub(crate) async fn commit_batch(
    ctx: &DbContext,
    commits: &[NarCommit],
    existing_object_keys: &HashSet<String>,
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

    let upserted = upsert_cached_paths(db, commits, &paths, existing_object_keys).await?;
    let references: Vec<Vec<String>> = upserted
        .iter()
        .map(Upserted::committed_store_path_references)
        .collect();
    let fresh: Vec<String> = hashes
        .iter()
        .zip(&upserted)
        .filter(|(_, u)| !u.was_backed)
        .map(|(h, _)| h.clone())
        .collect();

    commit_runtime_dependencies(ctx, txn, &references, &hashes).await?;

    let freshly_present =
        gradient_db::graph::reachability::producers_of_hashes_with_output_not_in_shared_build_unneeded_outputs(
            txn, &fresh,
        )
        .await?;
    let recounted = gradient_db::graph::reachability::producers_of_hashes(txn, &hashes).await?;
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

    let signing: Vec<(&StoredContentProducer, &StorePath, &Upserted)> = commits
        .iter()
        .zip(&paths)
        .zip(&upserted)
        .map(|((c, sp), u)| (&c.producer, sp, u))
        .collect();
    let signed = sign_into_caches(ctx, &signing).await?;
    ctx.request_signature_sweep_wake();

    Ok(upserted
        .into_iter()
        .zip(signed)
        .map(|(u, signed)| NarCommitted {
            cached_path: u.row.id,
            nar_object_storage_key: u.row.nar_object_storage_key,
            created: u.created,
            signed,
        })
        .collect())
}

async fn upsert_cached_paths(
    db: &WorkerDb,
    commits: &[NarCommit],
    paths: &[StorePath],
    existing_object_keys: &HashSet<String>,
) -> anyhow::Result<Vec<Upserted>> {
    let hashes: Vec<&str> = paths.iter().map(|sp| sp.hash()).collect();
    let mut existing: HashMap<String, Vec<MCachedPath>> = HashMap::new();
    for row in ECachedPath::find()
        .filter(CCachedPath::Hash.is_in(hashes))
        .order_by_asc(CCachedPath::Hash)
        .order_by_asc(CCachedPath::CreatedAt)
        .order_by_asc(CCachedPath::Id)
        .lock(LockType::NoKeyUpdate)
        .all(db)
        .await?
    {
        existing.entry(row.hash.clone()).or_default().push(row);
    }

    let mut upserted = Vec::with_capacity(commits.len());
    for (c, sp) in commits.iter().zip(paths) {
        let variants = existing.remove(sp.hash()).unwrap_or_default();
        upserted.push(
            upsert_variant(db, sp.hash(), sp.name(), c, variants, existing_object_keys).await?,
        );
    }

    Ok(upserted)
}

async fn commit_runtime_dependencies(
    ctx: &DbContext,
    txn: &sea_orm::DatabaseTransaction,
    references: &[Vec<String>],
    hashes: &[String],
) -> anyhow::Result<()> {
    let referencing: Vec<(&Vec<String>, &String)> = references
        .iter()
        .zip(hashes)
        .filter(|(references, _)| !references.is_empty())
        .collect();
    let candidates: Vec<String> = referencing.iter().map(|(_, h)| (*h).clone()).collect();
    if candidates.is_empty()
        || gradient_db::graph::reachability::producers_of_hashes(txn, &candidates)
            .await?
            .is_empty()
    {
        return Ok(());
    }

    for (references, hash) in referencing {
        let producers =
            gradient_db::graph::reachability::producers_of_hashes(txn, std::slice::from_ref(hash))
                .await?;
        if producers.is_empty() {
            continue;
        }
        let referenced =
            gradient_db::graph::runtime_dependencies::producers_of_tokens(txn, references).await?;
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
    let cached_path = committed.cached_path;
    ctx.shutdown.spawn(async move {
        match gradient_db::caches::debug_info::index_cached_path(&db, &nar_storage, cached_path)
            .await
        {
            Ok(0) => {}
            Ok(count) => debug!(%cached_path, count, "indexed debug-info build ids"),
            Err(e) => warn!(%cached_path, error = %e, "failed to index debug info"),
        }
    });
}

pub(crate) async fn find_object_keys_in_storage_at_batch_start(
    ctx: &DbContext,
    commits: &[NarCommit],
) -> HashSet<String> {
    let probes = commits.iter().filter_map(|commit| {
        let NarCommitContent::ServerComputedNarAndCompressedFileHashesAndSizes(facts) =
            &commit.content
        else {
            return None;
        };
        let sp = StorePath::parse_stored(&commit.store_path).ok()?;
        let key =
            facts.object_storage_key_for_store_path_hash_and_compressed_file_sha256(sp.hash());
        Some(async move {
            let found = ctx.storage.nar_storage.exists(&key).await;
            (key, found)
        })
    });

    let mut found = HashSet::new();
    for (key, probe) in futures::future::join_all(probes).await {
        match probe {
            Ok(true) => {
                found.insert(key);
            }
            Ok(false) => {}
            Err(e) => {
                warn!(nar_object_storage_key = %key, error = %e, "failed to check that the uploaded object is present, so the commit cannot proceed.")
            }
        }
    }
    found
}

pub(crate) async fn delete_uploaded_object_if_no_row_references_it(
    ctx: &DbContext,
    commit: &NarCommit,
    committed: &NarCommitted,
) {
    let NarCommitContent::ServerComputedNarAndCompressedFileHashesAndSizes(facts) = &commit.content
    else {
        return;
    };
    let Ok(sp) = StorePath::parse_stored(&commit.store_path) else {
        return;
    };
    let placed = facts.object_storage_key_for_store_path_hash_and_compressed_file_sha256(sp.hash());
    if placed == committed.nar_object_storage_key {
        return;
    }

    let referenced = ECachedPath::find()
        .filter(CCachedPath::NarObjectStorageKey.eq(placed.as_str()))
        .count(&ctx.worker_db)
        .await;
    match referenced {
        Ok(0) => match ctx.storage.nar_storage.delete(&placed).await {
            Ok(()) => {
                debug!(store_path = %commit.store_path, nar_object_storage_key = %placed, "Deleted the uploaded object. The same content is already in storage.")
            }
            Err(e) => {
                warn!(store_path = %commit.store_path, nar_object_storage_key = %placed, error = %e, "Deletion of an uploaded object failed for content that's already stored. Orphan cleanup passes delete that object later.")
            }
        },
        Ok(_) => {}
        Err(e) => {
            warn!(store_path = %commit.store_path, nar_object_storage_key = %placed, error = %e, "A check for rows that still use the uploaded object failed. Orphan cleanup can remove that object later.")
        }
    }
}

struct Upserted {
    created: bool,
    was_backed: bool,
    row: MCachedPath,
}

impl Upserted {
    fn committed_store_path_references(&self) -> Vec<String> {
        self.row
            .references
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }
}

async fn upsert_cached_path(
    db: &WorkerDb,
    hash: &str,
    package: &str,
    c: &NarCommit,
    existing_object_keys: &HashSet<String>,
) -> anyhow::Result<Upserted> {
    let variants = ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .order_by_asc(CCachedPath::CreatedAt)
        .order_by_asc(CCachedPath::Id)
        .lock(LockType::NoKeyUpdate)
        .all(db)
        .await?;

    upsert_variant(db, hash, package, c, variants, existing_object_keys).await
}

async fn upsert_variant(
    db: &WorkerDb,
    hash: &str,
    package: &str,
    c: &NarCommit,
    variants: Vec<MCachedPath>,
    existing_object_keys: &HashSet<String>,
) -> anyhow::Result<Upserted> {
    if let StoredContentProducer::Worker { credentials, .. } = &c.producer
        && credentials.worker_registrations.is_empty()
        && credentials.team_worker.is_none()
    {
        anyhow::bail!(
            "rejected commit of {} by worker: worker has no credentials for the job's project",
            c.store_path
        );
    }

    let was_backed = !variants.is_empty();
    let references_text = join_sorted_deduplicated_references_with_spaces(&c.references);
    let upserted = match &c.content {
        NarCommitContent::ExistingServerVerifiedCachedPathId(id) => {
            let row = variants
                .into_iter()
                .find(|v| v.id == *id && v.server_computed_hashes_and_sizes)
                .with_context(|| format!("The cached path in the commit can be missing, unverified on the server, or from another store path. Upload the content again. ({})", c.store_path))?;
            anyhow::ensure!(
                row.references.as_deref().unwrap_or_default() == references_text
                    && row.ca.as_deref() == c.content_address(),
                "Rejected commit. Its references or content address differ from the stored variant. ({})",
                c.store_path
            );
            Upserted {
                created: false,
                was_backed,
                row,
            }
        }
        NarCommitContent::ServerComputedNarAndCompressedFileHashesAndSizes(facts) => {
            let key = facts.object_storage_key_for_store_path_hash_and_compressed_file_sha256(hash);
            let matching = variants.into_iter().find(|v| {
                v.is_server_verified_and_matches_nar_hash_references_text_and_content_address(
                    facts.nar_hash(),
                    &references_text,
                    c.content_address(),
                )
            });
            anyhow::ensure!(
                matching.is_some() || existing_object_keys.contains(&key),
                "The commit failed. The uploaded object didn't exist in storage when the commit started. A deletion after the upload can explain the missing object. Upload the content again. ({})",
                c.store_path
            );
            // The identity of a variant is always the uncompressed NAR, not the compressed file. Stored variants hold their own object, key and file hash for all later producers. Deletion of the object that such a commit placed is the writer's task.
            match matching {
                Some(row) => Upserted {
                    created: false,
                    was_backed,
                    row,
                },
                None => Upserted {
                    created: true,
                    was_backed,
                    row: new_row(hash, package, key, facts, &references_text, c)
                        .into_active_model()
                        .insert(db)
                        .await?,
                },
            }
        }
    };

    let producers =
        store_path_content_producer_rows_for_variant(db, upserted.row.id, &c.producer).await?;
    if !producers.is_empty() {
        store_path_content_producer::Entity::insert_many(producers)
            .on_conflict(OnConflict::new().do_nothing().to_owned())
            .exec_without_returning(db)
            .await?;
    }

    Ok(upserted)
}

async fn store_path_content_producer_rows_for_variant(
    db: &WorkerDb,
    cached_path: CachedPathId,
    producer: &StoredContentProducer,
) -> anyhow::Result<Vec<store_path_content_producer::ActiveModel>> {
    let row = |kind| store_path_content_producer::Model {
        id: StorePathContentProducerId::now_v7(),
        cached_path,
        kind,
        created_at: now(),
        ..Default::default()
    };
    let build_upload_rows_for_cache_ids = |caches: &[CacheId], user, api_key, channel| {
        let upload = store_path_content_producer::Model {
            uploaded_by_user: user,
            uploaded_by_api_key: api_key,
            upload_channel: Some(channel),
            ..row(StoredContentVariantProducerKind::UploadedByUserOrApiKey)
        };
        caches
            .iter()
            .map(|cache| store_path_content_producer::Model {
                id: StorePathContentProducerId::now_v7(),
                cache: Some(*cache),
                ..upload.clone()
            })
            .collect()
    };

    let rows: Vec<store_path_content_producer::Model> = match producer {
        StoredContentProducer::Worker {
            credentials,
            dispatched_job,
            ..
        } => {
            let worker = store_path_content_producer::Model {
                dispatched_job: *dispatched_job,
                ..row(StoredContentVariantProducerKind::BuiltInDispatchedBuildJob)
            };
            let registrations = credentials.worker_registrations.iter().map(|registration| {
                store_path_content_producer::Model {
                    id: StorePathContentProducerId::now_v7(),
                    worker_registration: Some(*registration),
                    ..worker.clone()
                }
            });
            let team_worker =
                credentials
                    .team_worker
                    .map(|team_worker| store_path_content_producer::Model {
                        id: StorePathContentProducerId::now_v7(),
                        team_worker: Some(team_worker),
                        ..worker.clone()
                    });
            registrations.chain(team_worker).collect()
        }
        StoredContentProducer::Upload {
            caches,
            user,
            api_key,
            channel,
        } => build_upload_rows_for_cache_ids(caches, *user, *api_key, *channel),
        StoredContentProducer::ProjectUserContentUpload {
            project,
            user,
            channel,
        } => {
            let mut rows = build_upload_rows_for_cache_ids(
                &get_project_sign_output_cache_ids(db, *project).await?,
                Some(*user),
                None,
                *channel,
            );
            rows.push(store_path_content_producer::Model {
                uploaded_by_user: Some(*user),
                upload_channel: Some(*channel),
                uploading_member_project_id: Some(*project),
                ..row(StoredContentVariantProducerKind::UploadedByProjectMember)
            });
            rows
        }
    };

    Ok(rows
        .into_iter()
        .map(IntoActiveModel::into_active_model)
        .collect())
}

fn new_row(
    hash: &str,
    package: &str,
    key: String,
    facts: &ServerComputedNarAndCompressedFileHashesAndSizes,
    references_text: &str,
    c: &NarCommit,
) -> MCachedPath {
    MCachedPath {
        id: CachedPathId::now_v7(),
        hash: hash.to_owned(),
        nar_object_storage_key: key,
        package: package.to_owned(),
        file_hash: facts.file_hash().to_owned(),
        file_size: Some(facts.file_size() as i64),
        nar_size: facts.nar_size() as i64,
        nar_hash: facts.nar_hash().to_owned(),
        deriver: c.deriver.clone(),
        ca: c.content_address().map(str::to_owned),
        references: Some(references_text.to_owned()),
        server_computed_hashes_and_sizes: true,
        created_at: now(),
        ..Default::default()
    }
}

impl StoredContentProducer {
    async fn commit_signing_caches(
        &self,
        db: &WorkerDb,
    ) -> anyhow::Result<(Option<ProjectId>, Vec<CacheId>, bool)> {
        let (project, is_project_upload_commit) = match self {
            Self::Upload { caches, .. } => return Ok((None, caches.clone(), false)),
            Self::ProjectUserContentUpload { project, .. } => (*project, true),
            Self::Worker { project, .. } => (*project, false),
        };
        let caches = get_project_build_output_cache_ids(db, project).await?;

        Ok((Some(project), caches, is_project_upload_commit))
    }
}

async fn sign_into_caches(
    ctx: &DbContext,
    committed: &[(&StoredContentProducer, &StorePath, &Upserted)],
) -> anyhow::Result<Vec<Vec<CacheId>>> {
    let db = &ctx.worker_db;
    let mut signed = vec![Vec::new(); committed.len()];
    let hashes: Vec<String> = committed
        .iter()
        .map(|(_, sp, _)| sp.hash().to_owned())
        .collect();
    let private = gradient_db::graph::reachability::private_output_hashes(db, &hashes).await?;
    let mut allowed_caches_per_committed_path = Vec::with_capacity(committed.len());
    for &(producer, _, upserted) in committed {
        let (project, targets, is_project_upload_commit) =
            producer.commit_signing_caches(db).await?;
        allowed_caches_per_committed_path.push(
            caches_allowed_to_sign_committed_store_path_content_variant(
                db,
                upserted.row.id,
                project,
                &targets,
                is_project_upload_commit,
            )
            .await?,
        );
    }
    let caches = share_lock_caches_for_signing_and_read_signatures_of_unlocked_caches(
        ctx,
        allowed_caches_per_committed_path
            .iter()
            .flatten()
            .map(|allowed| allowed.cache)
            .collect(),
        hashes,
    )
    .await?;
    let signers: HashMap<CacheId, CacheSigner> = caches
        .locked
        .iter()
        .filter_map(|cache| signer_for(ctx, cache).map(|signer| (cache.id, signer)))
        .collect();
    let mut rows = Vec::new();
    let ts = now();
    for (i, (&(_, sp, upserted), allowed)) in committed
        .iter()
        .zip(allowed_caches_per_committed_path)
        .enumerate()
    {
        let cached_path = upserted.row.id;
        let references = upserted.committed_store_path_references();
        let fingerprint =
            (!private.contains(sp.hash())).then(|| (sp.full(), upserted.row.nar_hash.clone()));
        for AllowedCache {
            cache,
            sign_allowed,
        } in allowed
        {
            if caches.signature_row_exists_in_unlocked_cache(cache, sp.hash()) {
                continue;
            }
            let signature = match (&fingerprint, signers.get(&cache)) {
                (Some((store_path, nar_hash)), Some(signer)) if sign_allowed => {
                    Some(signer.sign_stored_narinfo(
                        &ctx.cache_mapper(cache),
                        store_path,
                        nar_hash,
                        upserted.row.nar_size as u64,
                        &references,
                    ))
                }
                _ => None,
            };
            if signature.is_some() {
                signed[i].push(cache);
            }
            rows.push(
                MCachedPathSignature {
                    id: CachedPathSignatureId::now_v7(),
                    cached_path,
                    cache,
                    signature,
                    created_at: ts,
                    ..Default::default()
                }
                .into_active_model(),
            );
        }
    }
    insert_signatures(db, rows).await?;

    Ok(signed)
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

pub(crate) struct CachesLockedForSigningAndExistingSignatureRowsToExclude {
    pub(crate) locked: Vec<MCache>,
    existing_signature_rows_to_leave_out_during_key_rotation: HashSet<(CacheId, String)>,
}

impl CachesLockedForSigningAndExistingSignatureRowsToExclude {
    pub(crate) fn signature_row_exists_in_unlocked_cache(
        &self,
        cache: CacheId,
        store_path_hash: &str,
    ) -> bool {
        self.existing_signature_rows_to_leave_out_during_key_rotation
            .contains(&(cache, store_path_hash.to_owned()))
    }
}

/// Key rotations lock every signature row of that cache. An upsert onto an existing row of a cache
/// that could not be locked would wait for the rotation. That row is absent from the upsert.
pub(crate) async fn share_lock_caches_for_signing_and_read_signatures_of_unlocked_caches(
    ctx: &DbContext,
    caches: Vec<CacheId>,
    store_path_hashes: Vec<String>,
) -> anyhow::Result<CachesLockedForSigningAndExistingSignatureRowsToExclude> {
    let locked =
        gradient_db::caches::share_lock_caches_in_ascending_id_order_skipping_rows_locked_by_key_rotation_settings_edit_or_delete(
            &ctx.worker_db,
            caches.clone(),
        )
        .await
        .context("Lock several caches that signatures are written into")?;
    let unlocked: HashSet<CacheId> = caches
        .into_iter()
        .filter(|cache| !locked.iter().any(|locked| locked.id == *cache))
        .collect();
    if unlocked.is_empty() {
        return Ok(CachesLockedForSigningAndExistingSignatureRowsToExclude {
            locked,
            existing_signature_rows_to_leave_out_during_key_rotation: HashSet::new(),
        });
    }

    ctx.request_signature_sweep_wake();
    let existing_signature_rows_to_leave_out_during_key_rotation = ECachedPathSignature::find()
        .select_only()
        .column(CCachedPathSignature::Cache)
        .column_as(Expr::col(Alias::new("store_path_hash")), "store_path_hash")
        .filter(CCachedPathSignature::Cache.is_in(unlocked))
        .filter(Expr::col(Alias::new("store_path_hash")).is_in(store_path_hashes))
        .into_tuple::<(CacheId, String)>()
        .all(&ctx.worker_db)
        .await
        .context("read the signature rows of caches during key rotation")?
        .into_iter()
        .collect();
    Ok(CachesLockedForSigningAndExistingSignatureRowsToExclude {
        locked,
        existing_signature_rows_to_leave_out_during_key_rotation,
    })
}

pub(crate) async fn insert_signatures(
    db: &WorkerDb,
    rows: Vec<ACachedPathSignature>,
) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }

    ECachedPathSignature::insert_many(rows)
        .on_conflict(
            OnConflict::columns([Alias::new("cache"), Alias::new("store_path_hash")])
                .value(
                    CCachedPathSignature::Signature,
                    Expr::cust("coalesce(cached_path_signature.signature, excluded.signature)"),
                )
                .value(
                    CCachedPathSignature::ReferencesAreClaimed,
                    Expr::cust(
                        "cached_path_signature.references_are_claimed OR excluded.references_are_claimed",
                    ),
                )
                .action_and_where(Expr::cust(
                    "cached_path_signature.cached_path = excluded.cached_path",
                ))
                .to_owned(),
        )
        .exec(db)
        .await
        .context("insert the cache claim row")?;

    Ok(())
}
