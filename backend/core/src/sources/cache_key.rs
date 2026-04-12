/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::SourceError;
use crate::types::*;
use anyhow::Result;
use base64::{Engine, engine::general_purpose};
use ed25519_compact::{KeyPair, SecretKey};

/// Returns `(encrypted_private_key, public_key_b64)`.
/// The private key is the full 64-byte ed25519 keypair encrypted and base64-encoded.
/// The public key is the last 32 bytes of the keypair, base64-encoded in plaintext.
pub fn generate_signing_key(secret_file: String) -> Result<(String, String), SourceError> {
    let secret = crate::types::input::load_secret_bytes(&secret_file);

    let keypair = KeyPair::generate();
    // Base64-encode the full 64-byte keypair (seed || public key)
    let key_b64 = general_purpose::STANDARD.encode(*keypair);
    // Derive the standalone public key (last 32 bytes)
    let public_key_b64 = general_purpose::STANDARD.encode(*keypair.pk);

    let encrypted_private_key = crypter::encrypt_with_password(secret.expose(), key_b64.as_bytes())
        .ok_or(SourceError::CryptographicOperation)?;

    Ok((
        general_purpose::STANDARD.encode(&encrypted_private_key),
        public_key_b64,
    ))
}

pub fn format_cache_public_key(
    secret_file: String,
    cache: MCache,
    url: String,
) -> Result<String, SourceError> {
    // Use the stored public key when available; fall back to deriving it from
    // the encrypted private key for caches created before the split migration.
    let pubkey_b64 = if cache.public_key.is_empty() {
        let key_b64 = decrypt_signing_key(secret_file, cache.clone())?;
        let key_bytes = general_purpose::STANDARD
            .decode(key_b64.trim())
            .map_err(|e| SourceError::CacheKeyDecoding {
                cache: cache.name.clone(),
                reason: format!("Failed to base64-decode signing key: {}", e),
            })?;
        if key_bytes.len() < 32 {
            return Err(SourceError::KeyPairConversion);
        }
        general_purpose::STANDARD.encode(&key_bytes[key_bytes.len() - 32..])
    } else {
        cache.public_key.clone()
    };

    let base_url = url
        .replace("https://", "")
        .replace("http://", "")
        .replace(":", "-");

    Ok(format!("{}-{}:{}", base_url, cache.name, pubkey_b64))
}

pub fn decrypt_signing_key(secret_file: String, cache: MCache) -> Result<String, SourceError> {
    let secret = crate::types::input::load_secret_bytes(&secret_file);

    let encrypted_private_key = general_purpose::STANDARD
        .decode(cache.clone().private_key)
        .map_err(|e| SourceError::CacheKeyDecoding {
            cache: cache.name.clone(),
            reason: format!("{}. The private key in the cache appears to be corrupted or not properly base64-encoded.", e)
        })?;

    let decrypted_private_key = crypter::decrypt_with_password(secret.expose(), encrypted_private_key)
        .ok_or(SourceError::PrivateKeyDecryption)?;

    let decrypted_key_str =
        String::from_utf8(decrypted_private_key).map_err(|_| SourceError::KeyUtf8Conversion)?;

    Ok(decrypted_key_str)
}

pub fn format_cache_key(
    secret_file: String,
    cache: MCache,
    url: String,
) -> Result<String, SourceError> {
    let decrypted_key = decrypt_signing_key(secret_file, cache.clone())?;

    let base_url = url
        .replace("https://", "")
        .replace("http://", "")
        .replace(":", "-");

    Ok(format!(
        "{}-{}:{}",
        base_url,
        cache.name,
        decrypted_key.trim()
    ))
}

/// Signs a Nix narinfo fingerprint directly with the cache's Ed25519 key.
///
/// Fingerprint format: `1;{store_path};{nar_hash};{nar_size};{refs_sorted_comma}`
/// Returns a full signature token: `{key_name}:{base64_sig}`.
///
/// References should be bare store-path names (without `/nix/store/` prefix);
/// this function adds the prefix before sorting and joining.
pub fn sign_narinfo_fingerprint(
    secret_file: String,
    cache: MCache,
    serve_url: String,
    store_path: &str,
    nar_hash: &str,
    nar_size: u64,
    references: &[String],
) -> Result<String, SourceError> {
    let key_b64 = decrypt_signing_key(secret_file, cache.clone())?;
    let key_bytes = general_purpose::STANDARD
        .decode(key_b64.trim())
        .map_err(|e| SourceError::CacheKeyDecoding {
            cache: cache.name.clone(),
            reason: format!("Failed to base64-decode signing key: {}", e),
        })?;

    let secret_key =
        SecretKey::from_slice(&key_bytes).map_err(|_| SourceError::KeyPairConversion)?;

    let mut full_refs: Vec<String> = references
        .iter()
        .map(|r| {
            if r.starts_with("/nix/store/") {
                r.clone()
            } else {
                format!("/nix/store/{}", r)
            }
        })
        .collect();
    full_refs.sort();
    let refs_str = full_refs.join(",");

    let fingerprint = format!("1;{};{};{};{}", store_path, nar_hash, nar_size, refs_str);
    let sig = secret_key.sign(fingerprint.as_bytes(), None);
    let sig_b64 = general_purpose::STANDARD.encode(*sig);

    let base_url = serve_url
        .replace("https://", "")
        .replace("http://", "")
        .replace(":", "-");

    Ok(format!("{}-{}:{}", base_url, cache.name, sig_b64))
}
