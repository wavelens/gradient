/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Context as _;
use gradient_db::store_path_content_variant_trust::{
    SQL_SELECT_STORED_CONTENT_VARIANTS_WITH_REFERENCE_CLOSURE_FOR_PROJECT_CACHE_SIGNING,
    build_sql_for_content_variants_needing_signature_from_store_hash_project_seeds,
    content_variant_is_readable_by_project_sql,
    project_cache_row_has_active_read_write_subscription_that_signs_outputs_and_cache_has_private_signing_key,
};
use gradient_db::{DbContext, WorkerDb};
use gradient_entity::StorePath;
use gradient_sources::CacheSigner;
use gradient_types::ids::{CacheId, CachedPathId, CachedPathSignatureId};
use gradient_types::*;
use gradient_util::nix_hash::normalize_nar_hash;
use sea_orm::{
    AccessMode, ColumnTrait, ConnectionTrait, DatabaseTransaction, EntityTrait, FromQueryResult,
    IntoActiveModel, QueryFilter, Statement, TransactionTrait, Value,
};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::nar::{
    insert_signatures, share_lock_caches_for_signing_and_read_signatures_of_unlocked_caches,
    signer_for,
};

const INSERT_CHUNK: usize = 1000;

gradient_db::sql_fn! {
    CLAIMS_OF_EVALUATION = claims_of_evaluation_sql,
        params = [EvaluationId, DerivationIds(64)],
        tier = Bulk;

    CLAIMS_OF_PATHS = claims_of_paths_sql,
        params = [CachedPathHashes(64)],
        tier = Bulk;
}

fn claims_of_evaluation_sql() -> String {
    claims_sql("bj.evaluation = $1 AND bj.derivation = ANY($2::uuid[])")
}

fn claims_of_paths_sql() -> String {
    claims_sql("dout.hash = ANY($1::text[])")
}

fn claims_sql(jobs: &str) -> String {
    let claimable =
        build_sql_for_content_variants_needing_signature_from_store_hash_project_seeds(&format!(
            "SELECT DISTINCT dout.hash, t.project \
             FROM build_job bj \
             JOIN derivation_output dout ON dout.derivation = bj.derivation \
             JOIN evaluation e           ON e.id = bj.evaluation \
             JOIN task t                 ON t.id = e.task \
             WHERE {jobs}"
        ));
    format!(
        r#"
SELECT claimable.*
FROM ({claimable}) claimable
JOIN cache c ON c.id = claimable.cache
WHERE c.private_key <> ''
"#
    )
}

#[derive(FromQueryResult)]
struct Claim {
    cached_path: CachedPathId,
    hash: String,
    package: String,
    nar_hash: String,
    nar_size: i64,
    references: Option<String>,
    cache: CacheId,
    cached_path_has_signature: bool,
    reference_closure_established_at_cache_deletion_count: Option<i64>,
}

impl Claim {
    fn signature(&self, signer: &CacheSigner, mapper: &StorePathMapper) -> Vec<u8> {
        let references: Vec<String> = self
            .references
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect();

        signer.sign_stored_narinfo(
            mapper,
            &StorePath::from_hash_and_name_without_validation(&self.hash, &self.package).full(),
            &normalize_nar_hash(&self.nar_hash),
            self.nar_size as u64,
            &references,
        )
    }
}

pub(crate) async fn claim_reused_outputs(
    ctx: &DbContext,
    evaluation: EvaluationId,
    derivations: &[DerivationId],
) -> anyhow::Result<()> {
    if derivations.is_empty() {
        return Ok(());
    }

    let ids: Vec<uuid::Uuid> = derivations.iter().map(|d| d.into_inner()).collect();
    sign_claims(
        ctx,
        CLAIMS_OF_EVALUATION.bind([Value::Uuid(Some(evaluation.into_inner())), ids.into()]),
    )
    .await
    .map(drop)
}

pub(crate) async fn claim_committed_paths(
    ctx: &DbContext,
    hashes: &[String],
) -> anyhow::Result<()> {
    if hashes.is_empty() {
        return Ok(());
    }

    sign_claims(ctx, CLAIMS_OF_PATHS.bind([hashes.to_vec().into()]))
        .await
        .map(drop)
}

pub const PARENT_BUILD_PAGE_READ_STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEPENDENCY_OUTPUT_SIGNING_SELECT_STATEMENT_TIMEOUT: Duration = Duration::from_secs(60);

/// A row with a signature from outside the graph writer, for later insertion by the graph writer.
/// The row can store the public key of the cache at signing time. Private key material can never
/// enter the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureRowForGraphWriterInsert {
    pub cached_path: CachedPathId,
    pub cache: CacheId,
    pub signature: Option<Vec<u8>>,
    pub reference_closure_established_at_cache_deletion_count: Option<i64>,
    pub cache_public_key_at_signing: Option<String>,
}

#[derive(Debug, Default)]
pub struct DependencyOutputPublishRoundResult {
    pub last_handled_parent_build_id: Option<DerivationId>,
    pub stopped_at_deadline_with_parent_builds_remaining: bool,
    pub insert_messages_sent_to_graph_writer: usize,
    pub failed_page_count: usize,
    pub parent_build_ids_set_aside_in_publish_round: Vec<DerivationId>,
    pub failure: Option<anyhow::Error>,
}

pub const PARENT_BUILD_SKIP_DURATION_AFTER_ONLY_PARENT_PAGE_PUBLISH_FAILURE: Duration =
    Duration::from_secs(600);

#[derive(Debug, Default)]
pub(crate) struct PublishFailurePageSizeAndWaitingParentBuilds {
    pub(crate) parent_builds_per_page: Option<usize>,
    pub(crate) set_aside_end_by_parent_build: HashMap<DerivationId, Instant>,
}

impl PublishFailurePageSizeAndWaitingParentBuilds {
    pub(crate) fn next_page_size_after_failures(
        &self,
        maximum_parent_builds_per_page: usize,
    ) -> usize {
        let maximum = maximum_parent_builds_per_page.max(1);
        self.parent_builds_per_page
            .unwrap_or(maximum)
            .clamp(1, maximum)
    }

    pub(crate) fn drop_expired_set_asides_and_return_page_parent_builds_not_set_aside(
        &mut self,
        page: &[DerivationId],
        now: Instant,
    ) -> Vec<DerivationId> {
        self.set_aside_end_by_parent_build
            .retain(|_, set_aside_until| *set_aside_until > now);
        page.iter()
            .copied()
            .filter(|parent_build| {
                !self
                    .set_aside_end_by_parent_build
                    .contains_key(parent_build)
            })
            .collect()
    }
}

pub(crate) async fn begin_read_only_transaction_and_set_statement_timeout(
    db: &WorkerDb,
    statement_timeout: Duration,
) -> anyhow::Result<DatabaseTransaction> {
    let transaction = db
        .begin_with_config(None, Some(AccessMode::ReadOnly))
        .await?;
    transaction
        .execute_unprepared(&format!(
            "SET LOCAL statement_timeout = {}",
            statement_timeout.as_millis()
        ))
        .await?;
    Ok(transaction)
}

pub(crate) async fn read_page_of_parent_builds_with_dependency_outputs_to_publish(
    db: &WorkerDb,
    after: DerivationId,
    upto: DerivationId,
    parent_builds_per_page: usize,
) -> anyhow::Result<Vec<DerivationId>> {
    let transaction = begin_read_only_transaction_and_set_statement_timeout(
        db,
        PARENT_BUILD_PAGE_READ_STATEMENT_TIMEOUT,
    )
    .await?;
    if !gradient_db::store_path_content_variant_trust::has_project_cache_subscription_for_stored_input_signing(
        &transaction,
    )
    .await?
    {
        return Ok(Vec::new());
    }
    let page =
        gradient_db::store_path_content_variant_trust::list_parent_build_derivation_ids_with_dependency_outputs_to_sign_page(
            &transaction,
            after,
            upto,
            parent_builds_per_page,
        )
        .await?;
    transaction.commit().await?;
    Ok(page)
}

#[derive(Debug)]
pub struct UnfinishedRequestsError {
    pub error: anyhow::Error,
    pub unfinished: gradient_db::DependencyOutputSigningRequest,
}

pub(crate) async fn parent_builds_with_dependency_outputs_to_publish(
    db: &WorkerDb,
    requested_parent_builds: &[DerivationId],
    requested_dependencies: &[DerivationId],
) -> anyhow::Result<Vec<DerivationId>> {
    let transaction = begin_read_only_transaction_and_set_statement_timeout(
        db,
        PARENT_BUILD_PAGE_READ_STATEMENT_TIMEOUT,
    )
    .await?;
    if !gradient_db::store_path_content_variant_trust::has_project_cache_subscription_for_stored_input_signing(
        &transaction,
    )
    .await?
    {
        return Ok(Vec::new());
    }
    let mut candidates =
        gradient_db::store_path_content_variant_trust::list_parent_derivations_using_any_dependency(
            &transaction,
            requested_dependencies,
        )
        .await?;
    candidates.extend_from_slice(requested_parent_builds);
    candidates.sort_unstable();
    candidates.dedup();
    let mut parent_builds = Vec::new();
    for chunk in candidates
        .chunks(gradient_db::caches::demotion::MAX_PUBLISHED_PARENT_BUILDS_PER_SIGNING_PASS)
    {
        parent_builds.extend(
            gradient_db::store_path_content_variant_trust::list_parent_build_derivation_ids_with_dependency_outputs_to_sign(
                &transaction,
                chunk,
            )
            .await?,
        );
    }
    transaction.commit().await?;
    parent_builds.sort_unstable();
    Ok(parent_builds)
}

pub(crate) async fn select_and_sign_dependency_outputs_of_parent_build_page(
    ctx: &DbContext,
    parents: &[DerivationId],
) -> anyhow::Result<Vec<SignatureRowForGraphWriterInsert>> {
    if parents.is_empty() {
        return Ok(Vec::new());
    }

    let transaction = begin_read_only_transaction_and_set_statement_timeout(
        &ctx.worker_db,
        DEPENDENCY_OUTPUT_SIGNING_SELECT_STATEMENT_TIMEOUT,
    )
    .await?;
    let ids: Vec<uuid::Uuid> = parents.iter().map(|d| d.into_inner()).collect();
    let claims = query_claims(
        &transaction,
        SQL_SELECT_STORED_CONTENT_VARIANTS_WITH_REFERENCE_CLOSURE_FOR_PROJECT_CACHE_SIGNING
            .bind([ids.into()]),
    )
    .await?;
    let signers = claim_signers(ctx, &transaction, &claims).await?;
    transaction.commit().await?;
    let claims_with_usable_signing_key_in_target_cache: Vec<Claim> = claims
        .into_iter()
        .filter(|claim| {
            signers
                .get(&claim.cache)
                .is_some_and(|signer| signer.signer.is_some())
        })
        .collect();
    Ok(build_cache_signature_rows_for_claims_with_public_keys(
        ctx,
        &claims_with_usable_signing_key_in_target_cache,
        &signers,
    )
    .into_iter()
    .map(
        |(row, cache_public_key_at_signing)| SignatureRowForGraphWriterInsert {
            cached_path: row.cached_path,
            cache: row.cache,
            signature: row.signature,
            reference_closure_established_at_cache_deletion_count: row
                .reference_closure_established_at_cache_deletion_count,
            cache_public_key_at_signing,
        },
    )
    .collect())
}

pub(crate) async fn insert_signed_dependency_output_rows_in_graph_writer_transaction_after_allowed_recheck(
    ctx: &DbContext,
    rows: Vec<SignatureRowForGraphWriterInsert>,
) -> anyhow::Result<()> {
    let created_at = now();
    for chunk in rows.chunks(INSERT_CHUNK) {
        let still_publishable = recheck_signed_rows_still_publishable_and_share_lock_target_caches(
            &ctx.worker_db,
            chunk,
        )
        .await?;
        insert_signatures(
            &ctx.worker_db,
            still_publishable
                .iter()
                .map(|row| {
                    MCachedPathSignature {
                        id: CachedPathSignatureId::now_v7(),
                        cached_path: row.cached_path,
                        cache: row.cache,
                        signature: row.signature.clone(),
                        reference_closure_established_at_cache_deletion_count: row
                            .reference_closure_established_at_cache_deletion_count,
                        created_at,
                        ..Default::default()
                    }
                    .into_active_model()
                })
                .collect(),
        )
        .await?;
    }
    Ok(())
}

gradient_db::sql_fn! {
    SIGNED_ROWS_STILL_PUBLISHABLE_POSITIONS_SQL = signed_dependency_output_rows_still_publishable_sql,
        params = [CachedPathIds(64), CacheIds(64), Texts("", 64), Bools(true, 64)],
        tier = Bulk;
}

fn signed_dependency_output_rows_still_publishable_sql() -> String {
    format!(
        "SELECT signed_row_to_insert.position \
         FROM unnest($1::uuid[], $2::uuid[], $3::text[], $4::bool[]) WITH ORDINALITY \
              AS signed_row_to_insert(cached_path, cache, cache_public_key_at_signing, has_signature, position) \
         JOIN cache cache_row_for_signed_claim ON cache_row_for_signed_claim.id = signed_row_to_insert.cache \
         WHERE (NOT signed_row_to_insert.has_signature \
                OR cache_row_for_signed_claim.public_key = signed_row_to_insert.cache_public_key_at_signing) \
           AND EXISTS (SELECT 1 FROM project_cache signing_project_cache \
                       WHERE signing_project_cache.cache = signed_row_to_insert.cache \
                         AND {signing} AND {readable})",
        signing = project_cache_row_has_active_read_write_subscription_that_signs_outputs_and_cache_has_private_signing_key(
            "signing_project_cache"
        ),
        readable = content_variant_is_readable_by_project_sql(
            "signed_row_to_insert.cached_path",
            "signing_project_cache.project",
            "signed_row_to_insert.cache",
        ),
    )
}

async fn recheck_signed_rows_still_publishable_and_share_lock_target_caches(
    db: &WorkerDb,
    rows: &[SignatureRowForGraphWriterInsert],
) -> anyhow::Result<Vec<SignatureRowForGraphWriterInsert>> {
    let caches_locked_by_share_lock_call =
        gradient_db::caches::share_lock_caches_in_ascending_id_order_skipping_rows_locked_by_key_rotation_settings_edit_or_delete(
            db,
            rows.iter().map(|row| row.cache).collect(),
        )
        .await
        .context("Lock the target caches for the signed rows of dependency outputs.")?;
    let rows: Vec<&SignatureRowForGraphWriterInsert> = rows
        .iter()
        .filter(|row| {
            caches_locked_by_share_lock_call
                .iter()
                .any(|cache| cache.id == row.cache)
        })
        .collect();
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let cached_paths: Vec<uuid::Uuid> = rows
        .iter()
        .map(|row| row.cached_path.into_inner())
        .collect();
    let caches: Vec<uuid::Uuid> = rows.iter().map(|row| row.cache.into_inner()).collect();
    let signing_public_keys: Vec<String> = rows
        .iter()
        .map(|row| row.cache_public_key_at_signing.clone().unwrap_or_default())
        .collect();
    let has_signature: Vec<bool> = rows.iter().map(|row| row.signature.is_some()).collect();
    let still_publishable_positions = db
        .query_all_raw(SIGNED_ROWS_STILL_PUBLISHABLE_POSITIONS_SQL.bind([
            cached_paths.into(),
            caches.into(),
            signing_public_keys.into(),
            has_signature.into(),
        ]))
        .await
        .context("Re-check signed rows for dependency outputs before insert.")?
        .iter()
        .map(|row| row.try_get::<i64>("", "position"))
        .collect::<Result<HashSet<i64>, _>>()?;
    Ok(rows
        .into_iter()
        .zip(1_i64..)
        .filter(|(_, position)| still_publishable_positions.contains(position))
        .map(|(row, _)| row.clone())
        .collect())
}

async fn sign_claims(ctx: &DbContext, claims: Statement) -> anyhow::Result<usize> {
    let mut claims = query_claims(&ctx.worker_db, claims).await?;
    let caches = share_lock_caches_for_signing_and_read_signatures_of_unlocked_caches(
        ctx,
        sorted_distinct_cache_ids_of_claims(&claims),
        claims.iter().map(|claim| claim.hash.clone()).collect(),
    )
    .await?;
    claims.retain(|claim| !caches.signature_row_exists_in_unlocked_cache(claim.cache, &claim.hash));
    let signers = build_signing_keys_by_cache_id(ctx, &caches.locked);
    insert_cached_path_signature_rows_for_claims_and_return_new_signature_count(
        ctx, &claims, &signers,
    )
    .await
}

async fn query_claims<C: ConnectionTrait>(db: &C, claims: Statement) -> anyhow::Result<Vec<Claim>> {
    Claim::find_by_statement(claims)
        .all(db)
        .await
        .context("read the cached outputs left to claim")
}

struct BinaryCacheClaimSigningKeys {
    signer: Option<CacheSigner>,
    public_key: String,
}

fn build_cache_signature_rows_for_claims_with_public_keys(
    ctx: &DbContext,
    claims: &[Claim],
    signers: &HashMap<CacheId, BinaryCacheClaimSigningKeys>,
) -> Vec<(MCachedPathSignature, Option<String>)> {
    let created_at = now();
    claims
        .iter()
        .filter(|claim| {
            !claim.cached_path_has_signature
                || claim
                    .reference_closure_established_at_cache_deletion_count
                    .is_some()
        })
        .map(|claim| {
            let signer = signers
                .get(&claim.cache)
                .filter(|_| !claim.cached_path_has_signature)
                .and_then(|signer| signer.signer.as_ref().map(|key| (key, &signer.public_key)));
            let row = MCachedPathSignature {
                id: CachedPathSignatureId::now_v7(),
                cached_path: claim.cached_path,
                cache: claim.cache,
                signature: signer
                    .map(|(key, _)| claim.signature(key, &ctx.cache_mapper(claim.cache))),
                reference_closure_established_at_cache_deletion_count: claim
                    .reference_closure_established_at_cache_deletion_count,
                created_at,
                ..Default::default()
            };
            (row, signer.map(|(_, public_key)| public_key.clone()))
        })
        .collect()
}

async fn insert_cached_path_signature_rows_for_claims_and_return_new_signature_count(
    ctx: &DbContext,
    claims: &[Claim],
    signers: &HashMap<CacheId, BinaryCacheClaimSigningKeys>,
) -> anyhow::Result<usize> {
    let db = &ctx.worker_db;
    let rows: Vec<MCachedPathSignature> =
        build_cache_signature_rows_for_claims_with_public_keys(ctx, claims, signers)
            .into_iter()
            .map(|(row, _)| row)
            .collect();

    let newly_signed_cache_signature_row_count =
        rows.iter().filter(|row| row.signature.is_some()).count();
    for chunk in rows.chunks(INSERT_CHUNK) {
        insert_signatures(
            db,
            chunk
                .iter()
                .cloned()
                .map(IntoActiveModel::into_active_model)
                .collect(),
        )
        .await?;
    }

    Ok(newly_signed_cache_signature_row_count)
}

async fn claim_signers<C: ConnectionTrait>(
    ctx: &DbContext,
    db: &C,
    claims: &[Claim],
) -> anyhow::Result<HashMap<CacheId, BinaryCacheClaimSigningKeys>> {
    if claims.is_empty() {
        return Ok(HashMap::new());
    }

    let caches = ECache::find()
        .filter(CCache::Id.is_in(sorted_distinct_cache_ids_of_claims(claims)))
        .all(db)
        .await
        .context("load the caches a claim signs into")?;
    Ok(build_signing_keys_by_cache_id(ctx, &caches))
}

fn sorted_distinct_cache_ids_of_claims(claims: &[Claim]) -> Vec<CacheId> {
    let mut ids: Vec<CacheId> = claims.iter().map(|c| c.cache).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn build_signing_keys_by_cache_id(
    ctx: &DbContext,
    caches: &[MCache],
) -> HashMap<CacheId, BinaryCacheClaimSigningKeys> {
    caches
        .iter()
        .map(|cache| {
            (
                cache.id,
                BinaryCacheClaimSigningKeys {
                    signer: signer_for(ctx, cache),
                    public_key: cache.public_key.clone(),
                },
            )
        })
        .collect()
}
