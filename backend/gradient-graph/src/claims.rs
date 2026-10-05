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

/// A path is claimed by every project whose signing task holds a job on its producer, the rule
/// the sign sweep repairs orphans by. Claims already signed are left out.
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
    fn signature(&self, signer: &CacheSigner) -> Option<Vec<u8>> {
        let (nar_hash, nar_size) = (self.nar_hash.as_deref()?, self.nar_size?);
        let references: Vec<String> = self
            .references
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect();

        Some(signer.sign_narinfo_raw(
            &StorePath::from_parts(&self.hash, &self.package).full(),
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
                    .and_then(|signer| claim.signature(signer)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Statement, Value};
    use std::collections::BTreeMap;

    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn claim_row(cached_path: CachedPathId, cache: CacheId) -> BTreeMap<String, Value> {
        BTreeMap::from([
            (
                "cached_path".to_owned(),
                Value::from(cached_path.into_inner()),
            ),
            ("hash".to_owned(), Value::from(HASH)),
            ("package".to_owned(), Value::from("hello-2.12")),
            (
                "nar_hash".to_owned(),
                Value::from(Some("sha256:def".to_owned())),
            ),
            ("nar_size".to_owned(), Value::from(Some(5_i64))),
            (
                "references".to_owned(),
                Value::from(Some(format!("{HASH}-hello-2.12"))),
            ),
            ("cache".to_owned(), Value::from(cache.into_inner())),
        ])
    }

    fn cache_row(id: CacheId, private_key: String) -> MCache {
        MCache {
            id,
            name: format!("cache-{id}"),
            private_key,
            created_at: now(),
            ..Default::default()
        }
    }

    fn signing_key(secret: &str) -> String {
        gradient_sources::generate_signing_key(secret)
            .expect("signing key")
            .0
    }

    fn secret_file() -> (tempfile::NamedTempFile, String) {
        use std::io::Write as _;

        let mut file = tempfile::NamedTempFile::new().expect("temp secret");
        file.write_all(b"test-secret-key-32-bytes-padding!")
            .expect("write secret");
        let path = file.path().to_string_lossy().to_string();
        (file, path)
    }

    fn signature_insert(log: &[Statement]) -> &Statement {
        log.iter()
            .find(|s| s.sql.contains("INSERT INTO \"cached_path_signature\""))
            .unwrap_or_else(|| panic!("the claims are written: {log:?}"))
    }

    fn signatures(insert: &Statement) -> Vec<Vec<u8>> {
        insert
            .values
            .as_ref()
            .expect("the insert binds values")
            .0
            .iter()
            .filter_map(|v| match v {
                Value::Bytes(Some(bytes)) => Some(bytes.to_vec()),
                _ => None,
            })
            .collect()
    }

    fn written(rows: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected: rows,
        }
    }

    #[tokio::test]
    async fn a_reused_output_is_signed_into_the_caches_of_the_evaluating_project() {
        let (_file, secret) = secret_file();
        let (cached_path, cache) = (CachedPathId::now_v7(), CacheId::now_v7());
        let evaluation = EvaluationId::now_v7();
        let derivation = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![claim_row(cached_path, cache)]])
            .append_query_results([vec![cache_row(cache, signing_key(&secret))]])
            .append_exec_results([written(1)])
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx_with_crypt_file(db, &secret).await;

        claim_reused_outputs(&ctx, evaluation, &[derivation])
            .await
            .expect("the claim lands");
        drop(ctx);

        let log = gradient_db::pool::raw_statements(pool.into_transaction_log());
        let seed = format!("{:?}", log[0].values);
        assert!(
            seed.contains(&evaluation.to_string()) && seed.contains(&derivation.to_string()),
            "the claim reads the jobs of this evaluation for the batch's derivations: {seed}"
        );
        let insert = signature_insert(&log);
        let bound = format!("{:?}", insert.values);
        assert!(
            bound.contains(&cached_path.to_string()) && bound.contains(&cache.to_string()),
            "{bound}"
        );
        assert!(
            matches!(signatures(insert).as_slice(), [sig] if sig.len() == 64),
            "the claim is signed, the narinfo endpoint skips unsigned rows: {insert:?}"
        );
        assert!(
            insert
                .sql
                .contains("coalesce(cached_path_signature.signature, excluded.signature)"),
            "an existing signature is kept: {}",
            insert.sql
        );
    }

    #[tokio::test]
    async fn a_committed_path_is_signed_into_the_caches_of_every_project_waiting_on_it() {
        let (_file, secret) = secret_file();
        let cached_path = CachedPathId::now_v7();
        let (first, second) = (CacheId::now_v7(), CacheId::now_v7());
        let key = signing_key(&secret);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                claim_row(cached_path, first),
                claim_row(cached_path, second),
            ]])
            .append_query_results([vec![cache_row(first, key.clone()), cache_row(second, key)]])
            .append_exec_results([written(2)])
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx_with_crypt_file(db, &secret).await;

        claim_committed_paths(&ctx, &[HASH.to_owned()])
            .await
            .expect("the claim lands");
        drop(ctx);

        let log = gradient_db::pool::raw_statements(pool.into_transaction_log());
        assert!(
            format!("{:?}", log[0].values).contains(HASH),
            "{:?}",
            log[0]
        );
        let insert = signature_insert(&log);
        let bound = format!("{:?}", insert.values);
        assert!(
            bound.contains(&first.to_string()) && bound.contains(&second.to_string()),
            "{bound}"
        );
        assert_eq!(signatures(insert).len(), 2, "{insert:?}");
    }

    #[tokio::test]
    async fn nothing_left_to_claim_reads_and_writes_nothing_else() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx(db).await;

        claim_committed_paths(&ctx, &[HASH.to_owned()])
            .await
            .expect("the claim lands");
        drop(ctx);

        assert_eq!(
            gradient_db::pool::statements(pool.into_transaction_log()).len(),
            1
        );
    }

    #[test]
    fn a_task_that_opted_out_of_signing_hands_out_no_claim() {
        for query in [&CLAIMS_OF_EVALUATION, &CLAIMS_OF_PATHS] {
            let sql = query.text();
            assert!(sql.contains("t.sign_cache"), "{sql}");
            assert!(sql.contains("s.signature IS NOT NULL"), "{sql}");
        }
    }
}
