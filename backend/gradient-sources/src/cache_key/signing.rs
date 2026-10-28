/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::format::decrypt_signing_key_bytes;
use super::{narinfo, realisation};
use crate::{SourceError, cache_key_host};
use base64::{Engine, engine::general_purpose};
use ed25519_compact::SecretKey;
use gradient_types::*;

pub struct CacheSigner {
    secret_key: SecretKey,
    cache_name: String,
    base_url: String,
}

impl CacheSigner {
    pub fn from_cache(
        secret_file: &str,
        cache: &MCache,
        serve_url: &str,
    ) -> Result<Self, SourceError> {
        let key_bytes = decrypt_signing_key_bytes(secret_file, cache)?;
        let secret_key = SecretKey::from_slice(key_bytes.expose())
            .map_err(|_| SourceError::KeyPairConversion)?;
        Ok(Self {
            secret_key,
            cache_name: cache.name.clone(),
            base_url: cache_key_host(serve_url),
        })
    }

    pub fn sign_narinfo(
        &self,
        store_path: &str,
        nar_hash: &str,
        nar_size: u64,
        references: &[String],
    ) -> String {
        let raw = self.sign_fingerprint(store_path, nar_hash, nar_size, references);
        let sig_b64 = general_purpose::STANDARD.encode(raw);
        format!("{}-{}:{}", self.base_url, self.cache_name, sig_b64)
    }

    pub fn sign_realisation(&self, drv_path: &str, output: &str, out_path: &str) -> String {
        let fingerprint = realisation::fingerprint(drv_path, output, out_path);
        let sig = self.secret_key.sign(fingerprint.as_bytes(), None);
        let sig_b64 = general_purpose::STANDARD.encode(*sig);
        format!("{}-{}:{}", self.base_url, self.cache_name, sig_b64)
    }

    pub fn sign_stored_narinfo(
        &self,
        mapper: &StorePathMapper,
        store_path: &str,
        nar_hash: &str,
        nar_size: u64,
        references: &[String],
    ) -> Vec<u8> {
        let original = |text: &str| {
            mapper
                .outbound_text(text)
                .unwrap_or_else(|_| text.to_owned())
        };
        let references: Vec<String> = references.iter().map(|r| original(r)).collect();
        self.sign_fingerprint(&original(store_path), nar_hash, nar_size, &references)
    }

    fn sign_fingerprint(
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
