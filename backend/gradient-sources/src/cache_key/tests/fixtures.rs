/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::{generate_signing_key, sign_narinfo_fingerprint};
use chrono::NaiveDateTime;
use gradient_types::MCache;
use std::io::Write;

pub fn temp_secret_file() -> (tempfile::NamedTempFile, String) {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(b"test-secret-key-32-bytes-padding!").unwrap();
    f.flush().unwrap();
    let path = f.path().to_string_lossy().to_string();
    (f, path)
}

pub fn make_cache(name: &str, public_key: &str, private_key: &str) -> MCache {
    MCache {
        id: gradient_types::ids::CacheId::nil(),
        name: name.to_string(),
        display_name: name.to_string(),
        description: String::new(),
        active: true,
        priority: 0,
        local_priority: None,
        public_key: public_key.to_string(),
        private_key: private_key.to_string(),
        public: false,
        created_by: gradient_types::ids::UserId::nil(),
        created_at: NaiveDateTime::default(),
        managed: false,
        max_storage_gb: 0,
    }
}

/// Build a narinfo body whose Sig line was produced by
/// `sign_narinfo_fingerprint`. Returns `(body, public_key_string)` where
/// `public_key_string` is the `{sig_key_name}:{pub_b64}` form that
/// `verify_narinfo_signature` consumes.
pub fn signed_narinfo_fixture() -> (String, String) {
    let (_f, path) = temp_secret_file();
    let (encrypted_priv, pub_b64) = generate_signing_key(&path).expect("generate failed");
    let cache = make_cache("upstream", &pub_b64, &encrypted_priv);
    let store_path = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-foo";
    let nar_hash = "sha256:0000000000000000000000000000000000000000000000000000";
    let nar_size: u64 = 1234;
    let refs = vec!["bbbb-b".to_string(), "cccc-c".to_string()];
    let sig = sign_narinfo_fingerprint(
        &path,
        cache,
        "https://cache.example.com".to_string(),
        store_path,
        nar_hash,
        nar_size,
        &refs,
    )
    .expect("sign failed");
    // sig is "{base_url}-{cache.name}:{sig_b64}" - the sig-key name is
    // everything before the last ':'.
    let (sig_key_name, _) = sig.rsplit_once(':').unwrap();
    let body = format!(
        "StorePath: {store_path}\n\
         URL: nar/xxxx.nar.xz\n\
         Compression: xz\n\
         FileHash: sha256:ffff\n\
         FileSize: 10\n\
         NarHash: {nar_hash}\n\
         NarSize: {nar_size}\n\
         References: {}\n\
         Sig: {sig}\n",
        refs.join(" "),
    );
    (body, format!("{sig_key_name}:{pub_b64}"))
}
