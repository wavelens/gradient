/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Eager per-path narinfo signing, shared by the worker `NarPush` handler and
//! the REST cache-upload endpoints so an uploaded path is servable immediately
//! rather than waiting for the periodic sweep.

use gradient_sources::CacheSigner;
use gradient_types::events::cache::NarSigned;
use gradient_types::ids::{CacheId, CachedPathId};
use gradient_types::*;
use gradient_util::nix_hash::normalize_nar_hash;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, IntoActiveModel,
    QueryFilter, Set,
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
/// Skips paths whose every producing task has `sign_cache=false` (the
/// reserved `build-request` task is always signable). Signing failures are
/// logged, never propagated: the NAR is already stored and the periodic sweep
/// re-signs whatever is left NULL. Each cache signed into is announced on `events`.
pub async fn sign_cached_path<C: ConnectionTrait>(
    db: &C,
    events: &EventBus,
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
    if hash.is_empty() || producing_tasks_all_private(db, hash).await {
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
        let cache = row.cache;
        let mut am = row.into_active_model();
        am.signature = Set(Some(sig));
        match am.update(db).await {
            Ok(_) => events.publish(NarSigned {
                cache,
                hash: hash.to_owned(),
            }),
            Err(e) => warn!(store_path, error = %e, "eager sign: persist signature failed"),
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

gradient_db::sql! {
    PRODUCER_SIGN_FLAGS = r#"
            SELECT count(*)::bigint AS producers,
                   count(*) FILTER (
                       WHERE p.sign_cache OR p.name = 'build-request'
                   )::bigint AS signable
            FROM derivation_output do_
            JOIN derivation d ON d.id = do_.derivation
            JOIN build_job b  ON b.derivation = d.id
            JOIN evaluation e ON e.id = b.evaluation
            JOIN task p    ON p.id = e.task
            WHERE do_.hash = $1
        "#,
        params = [CachedPathHash];
}

/// True iff the path is produced by at least one task and every producing
/// task has `sign_cache=false` - mirrors the sweep's skip gate. The reserved
/// per-project `build-request` task is always signable. Paths with no producing
/// task (`.drv` files, direct uploads) return false -> signed normally.
async fn producing_tasks_all_private<C: ConnectionTrait>(db: &C, hash: &str) -> bool {
    #[derive(FromQueryResult)]
    struct Flags {
        producers: i64,
        signable: i64,
    }

    let stmt = PRODUCER_SIGN_FLAGS.bind([hash.into()]);

    match Flags::find_by_statement(stmt).one(db).await {
        Ok(Some(f)) => f.producers > 0 && f.signable == 0,
        Ok(None) => false,
        Err(e) => {
            warn!(%hash, error = %e, "eager sign: producer-flag query failed; signing");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::ids::{CachedPathSignatureId, UserId};
    use gradient_types::EventBus;
    use gradient_types::events::{Event, cache::NarSigned};
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;
    use std::io::Write;

    const STORE_PATH: &str = "/nix/store/0c7m1b3d0bgalbq8w4nmh5pjm3kc2dfq-hello-2.12";
    const NAR_HASH: &str = "sha256:1b8m03r63zqhnjf7l5wnldhh7c134ap5vpj0850ymkq1iyzicy5s";

    fn secret_file() -> (tempfile::NamedTempFile, String) {
        let mut file = tempfile::NamedTempFile::new().expect("temp secret");
        file.write_all(b"test-secret-key-32-bytes-padding!")
            .expect("write secret");
        let path = file.path().to_string_lossy().to_string();
        (file, path)
    }

    fn cache(private_key: String) -> MCache {
        MCache {
            id: CacheId::now_v7(),
            name: "main".into(),
            display_name: "Main".into(),
            description: String::new(),
            active: true,
            priority: 0,
            local_priority: None,
            public_key: String::new(),
            private_key,
            public: true,
            created_by: UserId::now_v7(),
            created_at: chrono::Utc::now().naive_utc(),
            managed: false,
            max_storage_gb: 0,
        }
    }

    fn pending(cached_path: CachedPathId, cache: CacheId) -> MCachedPathSignature {
        MCachedPathSignature {
            id: CachedPathSignatureId::now_v7(),
            cached_path,
            cache,
            signature: None,
            last_fetched_at: None,
            fetch_count: 0,
            created_at: chrono::Utc::now().naive_utc(),
        }
    }

    fn no_producers() -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("producers".to_owned(), Value::BigInt(Some(0))),
            ("signable".to_owned(), Value::BigInt(Some(0))),
        ])
    }

    async fn signed_events(cache: MCache) -> Vec<NarSigned> {
        let (_file, secret) = secret_file();
        let cached_path = CachedPathId::now_v7();
        let row = pending(cached_path, cache.id);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![no_producers()]])
            .append_query_results([vec![row.clone()]])
            .append_query_results([vec![cache]])
            .append_query_results([vec![MCachedPathSignature {
                signature: Some(vec![1]),
                ..row
            }]])
            .into_connection();
        let events = EventBus::default();
        let mut rx = events.subscribe();

        sign_cached_path(
            &db,
            &events,
            &secret,
            "https://gradient.example",
            SignRequest {
                cached_path,
                store_path: STORE_PATH,
                nar_hash: NAR_HASH,
                nar_size: 1024,
                references: &[],
            },
        )
        .await;

        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|envelope| match &envelope.event {
                Event::CacheNarSigned(signed) => Some(signed.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_signed_upload_announces_the_cache_it_landed_in() {
        let (_file, secret) = secret_file();
        let (private_key, _) =
            gradient_sources::generate_signing_key(&secret).expect("signing key");
        let cache = cache(private_key);
        let id = cache.id;

        let signed = signed_events(cache).await;

        assert_eq!(
            signed,
            vec![NarSigned {
                cache: id,
                hash: "0c7m1b3d0bgalbq8w4nmh5pjm3kc2dfq".into(),
            }]
        );
    }

    #[tokio::test]
    async fn a_cache_without_a_key_announces_nothing() {
        assert!(signed_events(cache(String::new())).await.is_empty());
    }
}
