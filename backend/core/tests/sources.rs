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
    let encoded = base64::engine::general_purpose::STANDARD.encode(b"this_is_a_test_secret_key_32chars");
    secret_file.write_all(encoded.as_bytes()).unwrap();

    let (private_key, public_key) = generate_ssh_key(secret_file.path().to_str().unwrap().to_string()).unwrap();

    assert!(!private_key.is_empty());
    assert!(public_key.starts_with("ssh-ed25519 "), "public key should be OpenSSH ed25519 format");

    // private key must be base64-decodable (it is stored encrypted)
    base64::engine::general_purpose::STANDARD.decode(&private_key).unwrap();
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
