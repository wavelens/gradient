/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Eager per-path narinfo signing, shared by the worker `NarPush` handler and
//! the REST cache-upload endpoints so an uploaded path is servable immediately
//! rather than waiting for the periodic sweep.

use gradient_sources::CacheSigner;
use gradient_types::ids::{CacheId, CachedPathId};
use gradient_types::*;
use gradient_util::nix_hash::normalize_nar_hash;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, IntoActiveModel,
    QueryFilter, Set, Statement,
};
use std::collections::{HashMap, HashSet};
use tracing::warn;

/// One freshly cached path to sign, in the narinfo terms the fingerprint needs.
/// `nar_hash` may be in any recognised format; it is normalized to nix32 before
/// fingerprinting. `references` are in hash-name form (no `/nix/store/` prefix).
pub struct SignRequest<'a> {
    pub cached_path: CachedPathId,
    pub store_path: &'a str,
    pub nar_hash: &'a str,
    pub nar_size: i64,
    pub references: &'a [String],
}

/// Fill the pending `cached_path_signature` rows for one freshly cached path.
/// Skips paths whose every producing project has `sign_cache=false` (the
/// reserved `build-request` project is always signable). Signing failures are
/// logged, never propagated: the NAR is already stored and the periodic sweep
/// re-signs whatever is left NULL.
pub async fn sign_cached_path<C: ConnectionTrait>(
    db: &C,
    crypt_secret_file: &str,
    serve_url: &str,
    req: SignRequest<'_>,
) {
    let store_path = req.store_path;
    let hash = store_path
        .strip_prefix("/nix/store/")
        .unwrap_or(store_path)
        .split('-')
        .next()
        .unwrap_or("");
    if hash.is_empty() || producing_projects_all_private(db, hash).await {
        return;
    }

    let pending = match ECachedPathSignature::find()
        .filter(CCachedPathSignature::CachedPath.eq(req.cached_path))
        .filter(CCachedPathSignature::Signature.is_null())
        .all(db)
        .await
    {
        Ok(rows) if !rows.is_empty() => rows,
        Ok(_) => return,
        Err(e) => {
            warn!(store_path, error = %e, "eager sign: load pending signatures failed");
            return;
        }
    };

    // The just-stored references (hash-name form) are what the narinfo serve
    // path reconstructs the fingerprint from; `fingerprint` sorts them, so this
    // matches the sweep byte-for-byte.
    let nar_hash_nix32 = normalize_nar_hash(req.nar_hash);
    let nar_size = req.nar_size as u64;

    // One signer per distinct cache (one crypt-secret read + key decrypt each);
    // caches whose key is absent/undecodable are simply absent.
    let mut signers: HashMap<CacheId, CacheSigner> = HashMap::new();
    for cache_id in pending.iter().map(|r| r.cache).collect::<HashSet<_>>() {
        if let Some(signer) = build_signer(db, crypt_secret_file, serve_url, cache_id).await {
            signers.insert(cache_id, signer);
        }
    }

    for row in pending {
        let Some(signer) = signers.get(&row.cache) else {
            continue;
        };

        let sig = signer.sign_narinfo_raw(store_path, &nar_hash_nix32, nar_size, req.references);
        let mut am = row.into_active_model();
        am.signature = Set(Some(sig));
        if let Err(e) = am.update(db).await {
            warn!(store_path, error = %e, "eager sign: persist signature failed");
        }
    }
}

/// Build a signer for `cache_id`, or `None` when the cache is gone or its key is
/// empty/undecodable (the periodic sweep logs the same and skips those rows).
async fn build_signer<C: ConnectionTrait>(
    db: &C,
    crypt_secret_file: &str,
    serve_url: &str,
    cache_id: CacheId,
) -> Option<CacheSigner> {
    let cache = ECache::find_by_id(cache_id).one(db).await.ok().flatten()?;
    if cache.private_key.is_empty() {
        return None;
    }
    match CacheSigner::from_cache(crypt_secret_file, &cache, serve_url) {
        Ok(s) => Some(s),
        Err(e) => {
            warn!(cache_name = %cache.name, error = %e, "eager sign: failed to prepare signer");
            None
        }
    }
}

/// True iff the path is produced by at least one project and every producing
/// project has `sign_cache=false` - mirrors the sweep's skip gate. The reserved
/// per-org `build-request` project is always signable. Paths with no producing
/// project (`.drv` files, direct uploads) return false -> signed normally.
async fn producing_projects_all_private<C: ConnectionTrait>(db: &C, hash: &str) -> bool {
    #[derive(FromQueryResult)]
    struct Flags {
        producers: i64,
        signable: i64,
    }

    let stmt = Statement::from_sql_and_values(
        db.get_database_backend(),
        r#"
            SELECT count(*)::bigint AS producers,
                   count(*) FILTER (
                       WHERE p.sign_cache OR p.name = 'build-request'
                   )::bigint AS signable
            FROM derivation_output do_
            JOIN derivation d ON d.id = do_.derivation
            JOIN build_job b  ON b.derivation = d.id
            JOIN evaluation e ON e.id = b.evaluation
            JOIN project p    ON p.id = e.project
            WHERE do_.hash = $1
        "#,
        [hash.into()],
    );

    match Flags::find_by_statement(stmt).one(db).await {
        Ok(Some(f)) => f.producers > 0 && f.signable == 0,
        Ok(None) => false,
        Err(e) => {
            warn!(%hash, error = %e, "eager sign: producer-flag query failed; signing");
            false
        }
    }
}
