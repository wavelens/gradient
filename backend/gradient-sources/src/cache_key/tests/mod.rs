/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod fixtures;

use super::*;
use crate::SourceError;
use base64::{Engine, engine::general_purpose};
use fixtures::{make_cache, signed_narinfo_fixture, temp_secret_file};

#[test]
fn generate_decrypt_roundtrip() {
    let (_f, path) = temp_secret_file();
    let (encrypted_priv, pub_b64) = generate_signing_key(&path).expect("generate failed");
    let cache = make_cache("testcache", &pub_b64, &encrypted_priv);
    let decrypted = decrypt_signing_key(&path, cache).expect("decrypt failed");
    let bytes = general_purpose::STANDARD
        .decode(decrypted.expose().trim())
        .expect("base64 decode failed");
    assert_eq!(bytes.len(), 64, "ed25519 keypair is 64 bytes");
}

#[test]
fn format_cache_public_key_stored() {
    let (_f, path) = temp_secret_file();
    let (encrypted_priv, pub_b64) = generate_signing_key(&path).expect("generate failed");
    let cache = make_cache("mycache", &pub_b64, &encrypted_priv);
    let result = format_cache_public_key(&path, cache, "https://cache.example.com".to_string())
        .expect("format failed");
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
fn sign_narinfo_fingerprint_format() {
    let (_f, path) = temp_secret_file();
    let (encrypted_priv, pub_b64) = generate_signing_key(&path).expect("generate failed");
    let cache = make_cache("sigcache", &pub_b64, &encrypted_priv);
    let result = sign_narinfo_fingerprint(
        &path,
        cache,
        "https://cache.example.com".to_string(),
        "/nix/store/aaaa-hello",
        "sha256:AAAA",
        12345,
        &[],
    )
    .expect("sign failed");
    assert!(
        result.starts_with("cache.example.com-sigcache:"),
        "unexpected prefix: {result}"
    );
}

#[test]
fn cache_signer_matches_one_shot_signer() {
    let (_f, path) = temp_secret_file();
    let (encrypted_priv, pub_b64) = generate_signing_key(&path).expect("generate failed");
    let cache = make_cache("sigcache", &pub_b64, &encrypted_priv);
    let serve_url = "https://cache.example.com".to_string();

    let signer = CacheSigner::from_cache(&path, &cache, &serve_url).expect("signer build");

    let cases: &[(&str, &str, u64, &[&str])] = &[
        ("/nix/store/aaaa-a", "sha256:AAAA", 1, &[]),
        ("/nix/store/bbbb-b", "sha256:BBBB", 4242, &["aaaa-a"]),
        (
            "/nix/store/cccc-c",
            "sha256:CCCC",
            999999,
            &["zzzz-z", "aaaa-a", "/nix/store/yyyy-y"],
        ),
    ];

    for (sp, hash, size, refs) in cases {
        let refs_owned: Vec<String> = refs.iter().map(|s| (*s).to_string()).collect();
        let one_shot = sign_narinfo_fingerprint(
            &path,
            cache.clone(),
            serve_url.clone(),
            sp,
            hash,
            *size,
            &refs_owned,
        )
        .expect("one-shot sign");
        let batched = signer.sign_narinfo(sp, hash, *size, &refs_owned);
        assert_eq!(
            one_shot, batched,
            "CacheSigner output must match sign_narinfo_fingerprint for {sp}"
        );
    }
}

#[test]
fn cache_signer_rejects_bad_key_at_build_time() {
    let (_f, path) = temp_secret_file();
    let cache = make_cache("badcache", "", "!!!not-base64!!!");
    let res = CacheSigner::from_cache(&path, &cache, "https://cache.example.com");
    assert!(res.is_err(), "expected build error for corrupt key");
}

#[test]
fn sign_narinfo_sorts_references() {
    let (_f, path) = temp_secret_file();
    let (encrypted_priv, pub_b64) = generate_signing_key(&path).expect("generate failed");
    let cache = make_cache("sigcache", &pub_b64, &encrypted_priv);
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
        &path,
        cache.clone(),
        "https://cache.example.com".to_string(),
        "/nix/store/aaaa-hello",
        "sha256:AAAA",
        100,
        &refs_sorted,
    )
    .expect("sign failed");
    let sig2 = sign_narinfo_fingerprint(
        &path,
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
    let result = decrypt_signing_key(&path, cache);
    assert!(result.is_err(), "expected error for corrupted base64");
}

#[test]
fn format_cache_public_key_legacy_matches_stored() {
    let (_f, path) = temp_secret_file();
    let (encrypted_priv, pub_b64) = generate_signing_key(&path).expect("generate failed");
    let stored = make_cache("c", &pub_b64, &encrypted_priv);
    let legacy = make_cache("c", "", &encrypted_priv);
    let url = "https://cache.example.com".to_string();
    let r_stored = format_cache_public_key(&path, stored, url.clone()).unwrap();
    let r_legacy = format_cache_public_key(&path, legacy, url).unwrap();
    assert_eq!(r_stored, r_legacy);
}

#[test]
fn format_cache_public_key_strips_http_and_port() {
    let (_f, path) = temp_secret_file();
    let (encrypted_priv, pub_b64) = generate_signing_key(&path).expect("generate failed");
    let cache = make_cache("c", &pub_b64, &encrypted_priv);
    let r = format_cache_public_key(&path, cache, "http://cache.example.com:8080".to_string())
        .expect("format failed");
    assert!(
        r.starts_with("cache.example.com-8080-c:"),
        "unexpected format: {r}"
    );
}

#[test]
fn sign_narinfo_prefixes_bare_refs() {
    let (_f, path) = temp_secret_file();
    let (encrypted_priv, pub_b64) = generate_signing_key(&path).expect("generate failed");
    let cache = make_cache("c", &pub_b64, &encrypted_priv);
    let url = "https://cache.example.com".to_string();
    let bare = vec!["aaaa-a".to_string(), "bbbb-b".to_string()];
    let full = vec![
        "/nix/store/aaaa-a".to_string(),
        "/nix/store/bbbb-b".to_string(),
    ];
    let s1 = sign_narinfo_fingerprint(
        &path,
        cache.clone(),
        url.clone(),
        "/nix/store/x-y",
        "sha256:AAAA",
        42,
        &bare,
    )
    .unwrap();
    let s2 = sign_narinfo_fingerprint(
        &path,
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
    let (encrypted_priv, pub_b64) = generate_signing_key(&path).expect("generate failed");
    let cache = make_cache("c", &pub_b64, &encrypted_priv);
    let url = "https://cache.example.com".to_string();
    let s1 = sign_narinfo_fingerprint(
        &path,
        cache.clone(),
        url.clone(),
        "/nix/store/x-y",
        "sha256:AAAA",
        100,
        &[],
    )
    .unwrap();
    let s2 = sign_narinfo_fingerprint(&path, cache, url, "/nix/store/x-y", "sha256:AAAA", 101, &[])
        .unwrap();
    assert_ne!(s1, s2, "nar_size must participate in the fingerprint");
}

#[test]
fn sign_narinfo_store_path_affects_signature() {
    let (_f, path) = temp_secret_file();
    let (encrypted_priv, pub_b64) = generate_signing_key(&path).expect("generate failed");
    let cache = make_cache("c", &pub_b64, &encrypted_priv);
    let url = "https://cache.example.com".to_string();
    let s1 = sign_narinfo_fingerprint(
        &path,
        cache.clone(),
        url.clone(),
        "/nix/store/aaaa-a",
        "sha256:AAAA",
        1,
        &[],
    )
    .unwrap();
    let s2 = sign_narinfo_fingerprint(
        &path,
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
    let (_f, path) = temp_secret_file();
    let secret = gradient_types::input::load_secret_bytes(&path).unwrap();
    let short_b64 = general_purpose::STANDARD.encode(b"too short");
    let enc = crypter::encrypt_with_password(secret.expose(), short_b64.as_bytes()).unwrap();
    let enc_b64 = general_purpose::STANDARD.encode(enc);
    let cache = make_cache("c", "ignored", &enc_b64);
    let result = sign_narinfo_fingerprint(
        &path,
        cache,
        "https://cache.example.com".to_string(),
        "/nix/store/x-y",
        "sha256:AAAA",
        1,
        &[],
    );
    assert!(matches!(result, Err(SourceError::KeyPairConversion)));
}

#[test]
fn verify_narinfo_signature_accepts_valid() {
    let (path_info, public_key) = signed_narinfo_fixture();
    assert!(verify_narinfo_signature(&public_key, &path_info));
}

#[test]
fn verify_narinfo_signature_rejects_wrong_public_key() {
    let (path_info, _real_key) = signed_narinfo_fixture();
    let (_f, path) = temp_secret_file();
    let (_, other_pub_b64) = generate_signing_key(&path).expect("generate failed");
    let name = _real_key.rsplit_once(':').unwrap().0;
    let wrong = format!("{name}:{other_pub_b64}");
    assert!(!verify_narinfo_signature(&wrong, &path_info));
}

#[test]
fn verify_narinfo_signature_rejects_name_mismatch() {
    let (path_info, real_key) = signed_narinfo_fixture();
    let pub_b64 = real_key.rsplit_once(':').unwrap().1;
    let wrong = format!("other-name:{pub_b64}");
    assert!(!verify_narinfo_signature(&wrong, &path_info));
}

#[test]
fn verify_narinfo_signature_rejects_tampered_nar_size() {
    let (mut path_info, public_key) = signed_narinfo_fixture();
    path_info.nar_size = 9999;
    assert!(!verify_narinfo_signature(&public_key, &path_info));
}

#[test]
fn verify_narinfo_signature_rejects_tampered_store_path() {
    let (mut path_info, public_key) = signed_narinfo_fixture();
    path_info.store_path = "/nix/store/0c8xfp8wr4dhz8gm2qv5nk5r6dl7hzsk-evil".to_owned();
    assert!(!verify_narinfo_signature(&public_key, &path_info));
}

#[test]
fn verify_narinfo_signature_rejects_tampered_references() {
    let (mut path_info, public_key) = signed_narinfo_fixture();
    path_info.references.pop();
    assert!(!verify_narinfo_signature(&public_key, &path_info));
}

#[test]
fn verify_narinfo_signature_rejects_missing_sig() {
    let (mut path_info, public_key) = signed_narinfo_fixture();
    path_info.signature_lines.clear();
    assert!(!verify_narinfo_signature(&public_key, &path_info));
}

#[test]
fn verify_narinfo_signature_rejects_malformed_public_key() {
    let (path_info, _) = signed_narinfo_fixture();
    assert!(!verify_narinfo_signature("no-colon-here", &path_info));
    assert!(!verify_narinfo_signature("name:!!not-base64!!", &path_info));
}

#[test]
fn verify_narinfo_signature_rejects_all_narinfo_with_empty_public_key() {
    let (path_info, public_key) = signed_narinfo_fixture();
    let key_name = public_key.rsplit_once(':').unwrap().0;
    assert!(!verify_narinfo_signature("", &path_info));
    assert!(!verify_narinfo_signature(
        &format!("{key_name}:"),
        &path_info
    ));
}

#[test]
fn verify_narinfo_signature_accepts_multiple_sig_lines_when_one_matches_public_key() {
    let (mut path_info, public_key) = signed_narinfo_fixture();
    let key_name = public_key.rsplit_once(':').unwrap().0;
    let zero_signature = general_purpose::STANDARD.encode([0u8; 64]);
    path_info.signature_lines.splice(
        0..0,
        [
            format!("{key_name}:{zero_signature}"),
            format!("other-name:{zero_signature}"),
            format!("{key_name}:not-base64"),
        ],
    );
    assert!(verify_narinfo_signature(&public_key, &path_info));
}
