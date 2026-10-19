/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Context as _;
use gradient_db::DbContext;
use gradient_entity::StorePath;
use gradient_sources::CacheSigner;
use gradient_types::ids::{CacheId, CachedPathId, CachedPathSignatureId};
use gradient_types::*;
use gradient_util::nix_hash::normalize_nar_hash;
use sea_orm::{
    ColumnTrait, EntityTrait, FromQueryResult, IntoActiveModel, QueryFilter, Statement, Value,
};
use std::collections::HashMap;

use crate::nar::{insert_signatures, signer_for};

const INSERT_CHUNK: usize = 1000;

gradient_db::sql_fn! {
    CLAIMS_OF_EVALUATION = claims_of_evaluation_sql,
        params = [EvaluationId, DerivationIds(64)];

    CLAIMS_OF_PATHS = claims_of_paths_sql,
        params = [CachedPathHashes(64)];
}

fn claims_of_evaluation_sql() -> String {
    claims_sql("bj.evaluation = $1 AND bj.derivation = ANY($2::uuid[])")
}

fn claims_of_paths_sql() -> String {
    claims_sql("dout.hash = ANY($1::text[])")
}

fn claims_sql(jobs: &str) -> String {
    format!(
        r#"
WITH claim AS (
    SELECT DISTINCT dout.hash, pc.cache
    FROM build_job bj
    JOIN derivation_output dout ON dout.derivation = bj.derivation
    JOIN evaluation e           ON e.id = bj.evaluation
    JOIN task t                 ON t.id = e.task
    JOIN project_cache pc       ON pc.project = t.project
    WHERE {jobs} AND t.sign_cache
)
SELECT cp.id AS cached_path, cp.hash, cp.package, cp.nar_hash, cp.nar_size, cp."references",
       claim.cache
FROM claim
JOIN cached_path cp ON cp.hash = claim.hash
JOIN cache c        ON c.id = claim.cache
WHERE cp.file_hash IS NOT NULL AND c.private_key <> ''
  AND NOT EXISTS (
      SELECT 1 FROM cached_path_signature s
      WHERE s.cached_path = cp.id AND s.cache = claim.cache AND s.signature IS NOT NULL)
"#
    )
}

#[derive(FromQueryResult)]
struct Claim {
    cached_path: CachedPathId,
    hash: String,
    package: String,
    nar_hash: Option<String>,
    nar_size: Option<i64>,
    references: Option<String>,
    cache: CacheId,
}

impl Claim {
    fn signature(&self, signer: &CacheSigner, mapper: &StorePathMapper) -> Option<Vec<u8>> {
        let (nar_hash, nar_size) = (self.nar_hash.as_deref()?, self.nar_size?);
        let references: Vec<String> = self
            .references
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect();

        Some(signer.sign_stored_narinfo(
            mapper,
            &StorePath::from_stored(&self.hash, &self.package).full(),
            &normalize_nar_hash(nar_hash),
            nar_size as u64,
            &references,
        ))
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
}

pub(crate) async fn claim_committed_paths(
    ctx: &DbContext,
    hashes: &[String],
) -> anyhow::Result<()> {
    if hashes.is_empty() {
        return Ok(());
    }

    sign_claims(ctx, CLAIMS_OF_PATHS.bind([hashes.to_vec().into()])).await
}

async fn sign_claims(ctx: &DbContext, claims: Statement) -> anyhow::Result<()> {
    let db = &ctx.worker_db;
    let claims = Claim::find_by_statement(claims)
        .all(db)
        .await
        .context("read the cached outputs left to claim")?;

    if claims.is_empty() {
        return Ok(());
    }

    let signers = claim_signers(ctx, &claims).await?;
    let created_at = now();
    let rows: Vec<ACachedPathSignature> = claims
        .iter()
        .map(|claim| {
            MCachedPathSignature {
                id: CachedPathSignatureId::now_v7(),
                cached_path: claim.cached_path,
                cache: claim.cache,
                signature: signers
                    .get(&claim.cache)
                    .and_then(Option::as_ref)
                    .and_then(|signer| {
                        claim.signature(
                            signer,
                            &ctx.store_path_mapper(MappingContext::Cache(claim.cache)),
                        )
                    }),
                created_at,
                ..Default::default()
            }
            .into_active_model()
        })
        .collect();

    for chunk in rows.chunks(INSERT_CHUNK) {
        insert_signatures(db, chunk.to_vec()).await;
    }

    Ok(())
}

async fn claim_signers(
    ctx: &DbContext,
    claims: &[Claim],
) -> anyhow::Result<HashMap<CacheId, Option<CacheSigner>>> {
    let mut ids: Vec<CacheId> = claims.iter().map(|c| c.cache).collect();
    ids.sort_unstable();
    ids.dedup();

    Ok(ECache::find()
        .filter(CCache::Id.is_in(ids))
        .all(&ctx.worker_db)
        .await
        .context("load the caches a claim signs into")?
        .iter()
        .map(|cache| (cache.id, signer_for(ctx, cache)))
        .collect())
}
