/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::{SourceError, cache_key_host};
use base64::{Engine, engine::general_purpose};
use gradient_types::*;

pub fn format_cache_public_key(
    secret_file: &str,
    cache: MCache,
    url: String,
) -> Result<String, SourceError> {
    // Caches created before the split migration have no stored public key. Their key is derived
    // from the encrypted private key.
    let pubkey_b64 = if cache.public_key.is_empty() {
        let key_bytes = decrypt_signing_key_bytes(secret_file, &cache)?;
        let key_bytes = key_bytes.expose();
        if key_bytes.len() < 32 {
            return Err(SourceError::KeyPairConversion);
        }
        general_purpose::STANDARD.encode(&key_bytes[key_bytes.len() - 32..])
    } else {
        cache.public_key.clone()
    };

    let base_url = cache_key_host(&url);

    Ok(format!("{}-{}:{}", base_url, cache.name, pubkey_b64))
}

pub fn decrypt_signing_key(secret_file: &str, cache: MCache) -> Result<SecretString, SourceError> {
    let encrypted_private_key = general_purpose::STANDARD
        .decode(cache.clone().private_key)
        .map_err(|e| SourceError::CacheKeyDecoding {
            cache: cache.name.clone(),
            reason: format!("{}. The private key in the cache appears to be corrupted or not properly base64-encoded.", e)
        })?;

    crate::secret::decrypt_text(secret_file, encrypted_private_key)?
        .ok_or(SourceError::PrivateKeyDecryption)
}

pub(crate) fn decrypt_signing_key_bytes(
    secret_file: &str,
    cache: &MCache,
) -> Result<SecretBytes, SourceError> {
    let key_b64 = decrypt_signing_key(secret_file, cache.clone())?;
    general_purpose::STANDARD
        .decode(key_b64.expose().trim())
        .map(SecretBytes::new)
        .map_err(|e| SourceError::CacheKeyDecoding {
            cache: cache.name.clone(),
            reason: format!("Failed to base64-decode signing key: {}", e),
        })
}

pub fn format_cache_key(
    secret_file: &str,
    cache: MCache,
    url: String,
) -> Result<String, SourceError> {
    let decrypted_key = decrypt_signing_key(secret_file, cache.clone())?;

    let base_url = cache_key_host(&url);

    Ok(format!(
        "{}-{}:{}",
        base_url,
        cache.name,
        decrypted_key.expose().trim()
    ))
}
