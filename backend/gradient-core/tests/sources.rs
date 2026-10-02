/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use base64::Engine;
use gradient_sources::*;
use std::io::Write;
use tempfile::NamedTempFile;

// The valid Nix base32 alphabet is 0-9 plus abcdfghijklmnpqrsvwxyz, without e, o, t and u.
const H32: &str = "abcdfghijklmnpqrsvwxyz0123456789";
const H52: &str = "abcdfghijklmnpqrsvwxyz0123456789abcdfghijklmnpqrsvwx";

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
    // A narinfo name must have exactly 2 parts, and `.narinfo.zst` is carrying 3.
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
    let url = format!("{}.narinfo", &H32[..31]);
    assert!(get_hash_from_url(url).is_err());
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

#[test]
fn hash_from_path_extracts_hash_and_package() {
    let (hash, pkg) = get_hash_from_path("/nix/store/abc123-hello-1.0".to_string()).unwrap();
    assert_eq!(hash, "abc123");
    assert_eq!(pkg, "hello-1.0");
}

#[test]
fn hash_from_path_package_with_no_dash_rejected() {
    assert!(get_hash_from_path("/nix/store/abc123".to_string()).is_err());
}

#[test]
fn hash_from_path_too_few_segments_rejected() {
    assert!(get_hash_from_path("abc".to_string()).is_err());
    assert!(get_hash_from_path("/nix/store".to_string()).is_err());
}

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
fn generate_ssh_key_produces_valid_ed25519_keypair() {
    let mut secret_file = NamedTempFile::new().unwrap();
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(b"this_is_a_test_secret_key_32chars");
    secret_file.write_all(encoded.as_bytes()).unwrap();

    let (private_key, public_key) = generate_ssh_key(secret_file.path().to_str().unwrap()).unwrap();

    assert!(!private_key.is_empty());
    assert!(
        public_key.starts_with("ssh-ed25519 "),
        "public key should be OpenSSH ed25519 format"
    );

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
        generate_ssh_key(f.path().to_str().unwrap()).unwrap()
    };

    let (_, pub1) = make_key(b"secret_key_one__________________");
    let (_, pub2) = make_key(b"secret_key_two__________________");
    assert_ne!(pub1, pub2);
}
