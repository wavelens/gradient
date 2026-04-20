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

    let decrypted_private_key =
        crypter::decrypt_with_password(secret.expose(), encrypted_private_key)
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use std::io::Write;
    use uuid::Uuid;

    fn temp_secret_file() -> (tempfile::NamedTempFile, String) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"test-secret-key-32-bytes-padding!").unwrap();
        f.flush().unwrap();
        let path = f.path().to_string_lossy().to_string();
        (f, path)
    }

    fn make_cache(name: &str, public_key: &str, private_key: &str) -> MCache {
        MCache {
            id: Uuid::nil(),
            name: name.to_string(),
            display_name: name.to_string(),
            description: String::new(),
            active: true,
            priority: 0,
            public_key: public_key.to_string(),
            private_key: private_key.to_string(),
            public: false,
            created_by: Uuid::nil(),
            created_at: NaiveDateTime::default(),
            managed: false,
        }
    }

    #[test]
    fn generate_decrypt_roundtrip() {
        let (_f, path) = temp_secret_file();
        let (encrypted_priv, pub_b64) =
            generate_signing_key(path.clone()).expect("generate failed");
        let cache = make_cache("testcache", &pub_b64, &encrypted_priv);
        let decrypted = decrypt_signing_key(path, cache).expect("decrypt failed");
        // Decrypted should be base64-encoded 64-byte keypair
        let bytes = general_purpose::STANDARD
            .decode(decrypted.trim())
            .expect("base64 decode failed");
        assert_eq!(bytes.len(), 64, "ed25519 keypair is 64 bytes");
    }

    #[test]
    fn format_cache_public_key_stored() {
        let (_f, path) = temp_secret_file();
        let (encrypted_priv, pub_b64) =
            generate_signing_key(path.clone()).expect("generate failed");
        let cache = make_cache("mycache", &pub_b64, &encrypted_priv);
        let result = format_cache_public_key(path, cache, "https://cache.example.com".to_string())
            .expect("format failed");
        // format: {base_url}-{name}:{pubkey}
        assert!(
            result.contains("mycache"),
            "result should contain cache name"
        );
        assert!(
            result.contains(&pub_b64),
            "result should contain public key"
        );
        assert!(
            result.starts_with("cache.example.com-mycache:"),
            "unexpected format: {result}"
        );
    }

    #[test]
    fn format_cache_public_key_legacy() {
        // Empty public_key → derive from private key
        let (_f, path) = temp_secret_file();
        let (encrypted_priv, _pub_b64) =
            generate_signing_key(path.clone()).expect("generate failed");
        let cache = make_cache("legacy", "", &encrypted_priv);
        let result = format_cache_public_key(path, cache, "https://cache.example.com".to_string())
            .expect("format failed");
        assert!(
            result.starts_with("cache.example.com-legacy:"),
            "unexpected format: {result}"
        );
    }

    #[test]
    fn sign_narinfo_fingerprint_format() {
        let (_f, path) = temp_secret_file();
        let (encrypted_priv, pub_b64) =
            generate_signing_key(path.clone()).expect("generate failed");
        let cache = make_cache("sigcache", &pub_b64, &encrypted_priv);
        let result = sign_narinfo_fingerprint(
            path,
            cache,
            "https://cache.example.com".to_string(),
            "/nix/store/aaaa-hello",
            "sha256:AAAA",
            12345,
            &[],
        )
        .expect("sign failed");
        // Format: {base_url}-{name}:{base64_sig}
        assert!(
            result.starts_with("cache.example.com-sigcache:"),
            "unexpected prefix: {result}"
        );
    }

    #[test]
    fn sign_narinfo_sorts_references() {
        let (_f, path) = temp_secret_file();
        let (encrypted_priv, pub_b64) =
            generate_signing_key(path.clone()).expect("generate failed");
        let cache = make_cache("sigcache", &pub_b64, &encrypted_priv);
        // Sign once with sorted order, once with reversed order — signatures must match
        let refs_sorted = vec![
            "aaaa-a".to_string(),
            "bbbb-b".to_string(),
            "cccc-c".to_string(),
        ];
        let refs_reversed = vec![
            "cccc-c".to_string(),
            "bbbb-b".to_string(),
            "aaaa-a".to_string(),
        ];
        let sig1 = sign_narinfo_fingerprint(
            path.clone(),
            cache.clone(),
            "https://cache.example.com".to_string(),
            "/nix/store/aaaa-hello",
            "sha256:AAAA",
            100,
            &refs_sorted,
        )
        .expect("sign failed");
        let sig2 = sign_narinfo_fingerprint(
            path,
            cache,
            "https://cache.example.com".to_string(),
            "/nix/store/aaaa-hello",
            "sha256:AAAA",
            100,
            &refs_reversed,
        )
        .expect("sign failed");
        assert_eq!(sig1, sig2, "sorting refs should give identical signatures");
    }

    #[test]
    fn decrypt_corrupted_base64_fails() {
        let (_f, path) = temp_secret_file();
        let cache = make_cache("badcache", "", "!!!not-base64!!!");
        let result = decrypt_signing_key(path, cache);
        assert!(result.is_err(), "expected error for corrupted base64");
    }

    #[test]
    fn format_cache_public_key_legacy_matches_stored() {
        // Deriving the pubkey from the encrypted private key must yield exactly
        // the same result as reading cache.public_key — guards against the
        // "last 32 bytes" slice drifting.
        let (_f, path) = temp_secret_file();
        let (encrypted_priv, pub_b64) =
            generate_signing_key(path.clone()).expect("generate failed");
        let stored = make_cache("c", &pub_b64, &encrypted_priv);
        let legacy = make_cache("c", "", &encrypted_priv);
        let url = "https://cache.example.com".to_string();
        let r_stored = format_cache_public_key(path.clone(), stored, url.clone()).unwrap();
        let r_legacy = format_cache_public_key(path, legacy, url).unwrap();
        assert_eq!(r_stored, r_legacy);
    }

    #[test]
    fn format_cache_public_key_strips_http_and_port() {
        let (_f, path) = temp_secret_file();
        let (encrypted_priv, pub_b64) =
            generate_signing_key(path.clone()).expect("generate failed");
        let cache = make_cache("c", &pub_b64, &encrypted_priv);
        let r = format_cache_public_key(path, cache, "http://cache.example.com:8080".to_string())
            .expect("format failed");
        assert!(
            r.starts_with("cache.example.com-8080-c:"),
            "unexpected format: {r}"
        );
    }

    #[test]
    fn sign_narinfo_prefixes_bare_refs() {
        // Signing with bare store-path names should produce the same signature
        // as signing with fully-qualified `/nix/store/...` refs.
        let (_f, path) = temp_secret_file();
        let (encrypted_priv, pub_b64) =
            generate_signing_key(path.clone()).expect("generate failed");
        let cache = make_cache("c", &pub_b64, &encrypted_priv);
        let url = "https://cache.example.com".to_string();
        let bare = vec!["aaaa-a".to_string(), "bbbb-b".to_string()];
        let full = vec![
            "/nix/store/aaaa-a".to_string(),
            "/nix/store/bbbb-b".to_string(),
        ];
        let s1 = sign_narinfo_fingerprint(
            path.clone(),
            cache.clone(),
            url.clone(),
            "/nix/store/x-y",
            "sha256:AAAA",
            42,
            &bare,
        )
        .unwrap();
        let s2 = sign_narinfo_fingerprint(
            path,
            cache,
            url,
            "/nix/store/x-y",
            "sha256:AAAA",
            42,
            &full,
        )
        .unwrap();
        assert_eq!(s1, s2);
    }

    #[test]
    fn sign_narinfo_nar_size_affects_signature() {
        let (_f, path) = temp_secret_file();
        let (encrypted_priv, pub_b64) =
            generate_signing_key(path.clone()).expect("generate failed");
        let cache = make_cache("c", &pub_b64, &encrypted_priv);
        let url = "https://cache.example.com".to_string();
        let s1 = sign_narinfo_fingerprint(
            path.clone(),
            cache.clone(),
            url.clone(),
            "/nix/store/x-y",
            "sha256:AAAA",
            100,
            &[],
        )
        .unwrap();
        let s2 = sign_narinfo_fingerprint(
            path,
            cache,
            url,
            "/nix/store/x-y",
            "sha256:AAAA",
            101,
            &[],
        )
        .unwrap();
        assert_ne!(s1, s2, "nar_size must participate in the fingerprint");
    }

    #[test]
    fn sign_narinfo_store_path_affects_signature() {
        let (_f, path) = temp_secret_file();
        let (encrypted_priv, pub_b64) =
            generate_signing_key(path.clone()).expect("generate failed");
        let cache = make_cache("c", &pub_b64, &encrypted_priv);
        let url = "https://cache.example.com".to_string();
        let s1 = sign_narinfo_fingerprint(
            path.clone(),
            cache.clone(),
            url.clone(),
            "/nix/store/aaaa-a",
            "sha256:AAAA",
            1,
            &[],
        )
        .unwrap();
        let s2 = sign_narinfo_fingerprint(
            path,
            cache,
            url,
            "/nix/store/bbbb-b",
            "sha256:AAAA",
            1,
            &[],
        )
        .unwrap();
        assert_ne!(s1, s2);
    }

    #[test]
    fn sign_narinfo_short_key_fails() {
        // A decoded key shorter than ed25519 secret size must return KeyPairConversion.
        let (_f, path) = temp_secret_file();
        let secret = crate::types::input::load_secret_bytes(&path);
        let short_b64 = general_purpose::STANDARD.encode(b"too short");
        let enc = crypter::encrypt_with_password(secret.expose(), short_b64.as_bytes()).unwrap();
        let enc_b64 = general_purpose::STANDARD.encode(enc);
        let cache = make_cache("c", "ignored", &enc_b64);
        let result = sign_narinfo_fingerprint(
            path,
            cache,
            "https://cache.example.com".to_string(),
            "/nix/store/x-y",
            "sha256:AAAA",
            1,
            &[],
        );
        assert!(matches!(result, Err(SourceError::KeyPairConversion)));
    }
}
