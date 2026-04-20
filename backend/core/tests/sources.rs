/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Tests for `core::sources` — path/hash utilities and SSH key generation.

extern crate core as gradient_core;
use base64::Engine;
use gradient_core::sources::*;
use std::io::Write;
use tempfile::NamedTempFile;

// ── get_hash_from_url ────────────────────────────────────────────────────────

// Valid Nix base32 alphabet: 0-9 + abcdfghijklmnpqrsvwxyz (no e, o, t, u).
const H32: &str = "abcdfghijklmnpqrsvwxyz0123456789"; // 22+10 = 32
const H52: &str = "abcdfghijklmnpqrsvwxyz0123456789abcdfghijklmnpqrsvwx"; // 52

fn h32() -> String {
    H32.to_string()
}

#[test]
fn hash_from_url_narinfo_32_hash_ok() {
    let url = format!("{}.narinfo", h32());
    assert_eq!(get_hash_from_url(url).unwrap(), h32());
}

#[test]
fn hash_from_url_nar_52_hash_ok() {
    let url = format!("{}.nar", H52);
    assert_eq!(get_hash_from_url(url).unwrap(), H52);
}

#[test]
fn hash_from_url_nar_with_compression_ok() {
    let url = format!("{}.nar.zst", H52);
    assert_eq!(get_hash_from_url(url).unwrap(), H52);
}

#[test]
fn hash_from_url_narinfo_cannot_have_compression_suffix() {
    // narinfo must be 2 parts exactly — `.narinfo.zst` is 3 parts → rejected.
    let url = format!("{}.narinfo.zst", h32());
    assert!(get_hash_from_url(url).is_err());
}

#[test]
fn hash_from_url_single_part_rejected() {
    assert!(get_hash_from_url(h32()).is_err());
}

#[test]
fn hash_from_url_four_parts_rejected() {
    let url = format!("{}.nar.zst.extra", h32());
    assert!(get_hash_from_url(url).is_err());
}

#[test]
fn hash_from_url_wrong_hash_length_rejected() {
    // 31 chars
    let url = format!("{}.narinfo", &H32[..31]);
    assert!(get_hash_from_url(url).is_err());
    // 40 chars (git hash — not a Nix hash length)
    let url = format!("{}.nar", "a".repeat(40));
    assert!(get_hash_from_url(url).is_err());
}

#[test]
fn hash_from_url_wrong_extension_rejected() {
    let url = format!("{}.txt", h32());
    assert!(get_hash_from_url(url).is_err());
}

#[test]
fn hash_from_url_disallowed_base32_chars_rejected() {
    // 'e', 'o', 't', 'u' are not valid Nix base32.
    for bad in ['e', 'o', 't', 'u'] {
        let mut hash: String = "a".repeat(31);
        hash.push(bad);
        assert!(
            get_hash_from_url(format!("{}.narinfo", hash)).is_err(),
            "char {} should be rejected",
            bad
        );
    }
}

// ── get_hash_from_path ───────────────────────────────────────────────────────

#[test]
fn hash_from_path_extracts_hash_and_package() {
    let (hash, pkg) = get_hash_from_path("/nix/store/abc123-hello-1.0".to_string()).unwrap();
    assert_eq!(hash, "abc123");
    assert_eq!(pkg, "hello-1.0");
}

#[test]
fn hash_from_path_package_with_no_dash_rejected() {
    // Path `abc123` has no dash → can't split into hash-package.
    assert!(get_hash_from_path("/nix/store/abc123".to_string()).is_err());
}

#[test]
fn hash_from_path_too_few_segments_rejected() {
    assert!(get_hash_from_path("abc".to_string()).is_err());
    assert!(get_hash_from_path("/nix/store".to_string()).is_err());
}

// ── get_cache_nar_location ────────────────────────────────────────────────────

#[test]
fn nar_location_shards_by_first_two_hex_chars() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().to_str().unwrap().to_string();
    let hash = "ab1234567890abcdef1234567890abcdef123456".to_string();

    let path = get_cache_nar_location(base.clone(), hash.clone()).unwrap();

    assert!(path.starts_with(&base));
    assert!(path.contains("/ab/"));
    assert!(path.ends_with(".nar"));
}

#[test]
fn nar_compressed_location_has_zst_extension() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().to_str().unwrap().to_string();
    let hash = "ab1234567890abcdef1234567890abcdef123456".to_string();

    let path = get_cache_nar_compressed_location(base, hash).unwrap();

    assert!(path.ends_with(".nar.zst"));
}

// ── generate_ssh_key ──────────────────────────────────────────────────────────

#[test]
fn generate_ssh_key_produces_valid_ed25519_keypair() {
    let mut secret_file = NamedTempFile::new().unwrap();
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(b"this_is_a_test_secret_key_32chars");
    secret_file.write_all(encoded.as_bytes()).unwrap();

    let (private_key, public_key) =
        generate_ssh_key(secret_file.path().to_str().unwrap().to_string()).unwrap();

    assert!(!private_key.is_empty());
    assert!(
        public_key.starts_with("ssh-ed25519 "),
        "public key should be OpenSSH ed25519 format"
    );

    // private key must be base64-decodable (it is stored encrypted)
    base64::engine::general_purpose::STANDARD
        .decode(&private_key)
        .unwrap();
}

#[test]
fn generate_ssh_key_different_secrets_produce_different_keys() {
    let make_key = |secret: &[u8]| {
        let mut f = NamedTempFile::new().unwrap();
        let enc = base64::engine::general_purpose::STANDARD.encode(secret);
        f.write_all(enc.as_bytes()).unwrap();
        generate_ssh_key(f.path().to_str().unwrap().to_string()).unwrap()
    };

    let (_, pub1) = make_key(b"secret_key_one__________________");
    let (_, pub2) = make_key(b"secret_key_two__________________");
    assert_ne!(pub1, pub2);
}
