/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::format::decrypt_signing_key;
use super::narinfo;
use crate::{SourceError, cache_key_host};
use base64::{Engine, engine::general_purpose};
use ed25519_compact::SecretKey;
use gradient_types::*;
use gradient_util::sync::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

/// What a decrypted signer was derived from: a rotated key, a renamed cache or
/// another secret file is a different signer.
#[derive(PartialEq, Eq)]
struct Derivation {
    secret_file: String,
    private_key: String,
    cache_name: String,
    serve_url: String,
}

/// The key is Argon2-encrypted, so each decryption costs tens of milliseconds;
/// a signer is derived once per cache and reused until its row changes.
static SIGNERS: LazyLock<Mutex<HashMap<CacheId, Derived>>> = LazyLock::new(Default::default);

type Derived = (Derivation, Arc<CacheSigner>);

/// A pre-decrypted signer for a single cache, shared across many
/// signatures without re-reading the crypt-secret file or re-decrypting
/// the cache's private key.
pub struct CacheSigner {
    secret_key: SecretKey,
    cache_name: String,
    base_url: String,
}

impl CacheSigner {
    /// The signer of `cache`, decrypted on first use and shared until the row's
    /// key, name or the secret file changes.
    pub fn from_cache(
        secret_file: &str,
        cache: &MCache,
        serve_url: &str,
    ) -> Result<Arc<Self>, SourceError> {
        let derivation = Derivation {
            secret_file: secret_file.to_owned(),
            private_key: cache.private_key.clone(),
            cache_name: cache.name.clone(),
            serve_url: serve_url.to_owned(),
        };
        if let Some((from, signer)) = SIGNERS.lock().get(&cache.id)
            && *from == derivation
        {
            return Ok(Arc::clone(signer));
        }

        let signer = Arc::new(Self::decrypt(secret_file, cache, serve_url)?);
        SIGNERS
            .lock()
            .insert(cache.id, (derivation, Arc::clone(&signer)));
        Ok(signer)
    }

    fn decrypt(secret_file: &str, cache: &MCache, serve_url: &str) -> Result<Self, SourceError> {
        let key_b64 = decrypt_signing_key(secret_file, cache.clone())?;
        let key_bytes = general_purpose::STANDARD
            .decode(key_b64.trim())
            .map_err(|e| SourceError::CacheKeyDecoding {
                cache: cache.name.clone(),
                reason: format!("Failed to base64-decode signing key: {}", e),
            })?;
        let secret_key =
            SecretKey::from_slice(&key_bytes).map_err(|_| SourceError::KeyPairConversion)?;
        Ok(Self {
            secret_key,
            cache_name: cache.name.clone(),
            base_url: cache_key_host(serve_url),
        })
    }

    /// Sign a narinfo fingerprint and return the full narinfo signature
    /// token (`{key_name}:{base64_sig}`).
    pub fn sign_narinfo(
        &self,
        store_path: &str,
        nar_hash: &str,
        nar_size: u64,
        references: &[String],
    ) -> String {
        let raw = self.sign_narinfo_raw(store_path, nar_hash, nar_size, references);
        let sig_b64 = general_purpose::STANDARD.encode(raw);
        format!("{}-{}:{}", self.base_url, self.cache_name, sig_b64)
    }

    /// Sign a narinfo fingerprint and return the raw 64-byte Ed25519
    /// signature. Used when the caller stores the signature in `bytea` form
    /// and reconstructs the narinfo wire format on read.
    pub fn sign_narinfo_raw(
        &self,
        store_path: &str,
        nar_hash: &str,
        nar_size: u64,
        references: &[String],
    ) -> Vec<u8> {
        let fingerprint = narinfo::fingerprint(
            store_path,
            nar_hash,
            nar_size,
            references.iter().map(String::as_str),
        );
        let sig = self.secret_key.sign(fingerprint.as_bytes(), None);
        (*sig).to_vec()
    }
}

/// Signs a Nix narinfo fingerprint directly with the cache's Ed25519 key.
///
/// Fingerprint format: `1;{store_path};{nar_hash};{nar_size};{refs_sorted_comma}`
/// Returns a full signature token: `{key_name}:{base64_sig}`.
///
/// References should be bare store-path names (without `/nix/store/` prefix);
/// this function adds the prefix before sorting and joining.
///
/// One-shot wrapper around [`CacheSigner`] - prefer [`CacheSigner::from_cache`]
/// when signing many paths for the same cache.
pub fn sign_narinfo_fingerprint(
    secret_file: &str,
    cache: MCache,
    serve_url: String,
    store_path: &str,
    nar_hash: &str,
    nar_size: u64,
    references: &[String],
) -> Result<String, SourceError> {
    let signer = CacheSigner::from_cache(secret_file, &cache, &serve_url)?;
    Ok(signer.sign_narinfo(store_path, nar_hash, nar_size, references))
}
