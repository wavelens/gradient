/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Canonical encoding helpers for narinfo hash metadata.
//!
//! Gradient uses SHA-256 by default for new NAR/file hashes. BLAKE3 is
//! still accepted on the read path so rows uploaded while the BLAKE3
//! default was active (issue #132) keep resolving, and so upstream
//! caches that advertise either algorithm interoperate cleanly. All
//! on-disk DB columns hold values in `{algo}:{nix32}` form so the
//! narinfo URL's hash slug matches the column verbatim, no re-encoding
//! needed.

use base64::Engine as _;

/// Encode bytes using the Nix base32 alphabet (omits `e`, `o`, `t`, `u`).
pub fn nix32_encode(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";
    let len = (bytes.len() * 8 - 1) / 5 + 1;
    let mut out = String::with_capacity(len);
    for n in (0..len).rev() {
        let b = n * 5;
        let i = b / 8;
        let j = b % 8;
        let byte0 = bytes.get(i).copied().unwrap_or(0) as u32;
        let byte1 = bytes.get(i + 1).copied().unwrap_or(0) as u32;
        let c = ((byte0 >> j) | (byte1 << (8 - j))) & 0x1f;
        out.push(CHARS[c as usize] as char);
    }
    out
}

const HASH_ALGOS: &[&str] = &["sha256", "blake3"];

/// Converts any 32-byte hash representation (SRI `{algo}-{base64}`, nix32
/// `{algo}:{nix32}`, prefixed hex `{algo}:{hex}`, or bare hex with implicit
/// `sha256` for legacy callers) to the canonical `{algo}:{nix32}` form.
///
/// Recognised algorithms: `sha256`, `blake3` (both produce 32-byte digests
/// → 52-char nix32 / 64-char hex). Inputs that match no recognised form are
/// returned unchanged to preserve caller intent for sentinel values.
pub fn normalize_nar_hash(hash: &str) -> String {
    for algo in HASH_ALGOS {
        let sri_prefix = format!("{algo}-");
        if let Some(b64) = hash.strip_prefix(&sri_prefix)
            && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64)
            && bytes.len() == 32
        {
            return format!("{algo}:{}", nix32_encode(&bytes));
        }
    }
    for algo in HASH_ALGOS {
        let colon_prefix = format!("{algo}:");
        if let Some(rest) = hash.strip_prefix(&colon_prefix) {
            if rest.len() == 64
                && rest.chars().all(|c| c.is_ascii_hexdigit())
                && let Ok(bytes) = (0..32)
                    .map(|i| u8::from_str_radix(&rest[i * 2..i * 2 + 2], 16))
                    .collect::<Result<Vec<u8>, _>>()
            {
                return format!("{algo}:{}", nix32_encode(&bytes));
            }
            return hash.to_string();
        }
    }
    if hash.len() == 64
        && hash.chars().all(|c| c.is_ascii_hexdigit())
        && let Ok(bytes) = (0..32)
            .map(|i| u8::from_str_radix(&hash[i * 2..i * 2 + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
    {
        return format!("sha256:{}", nix32_encode(&bytes));
    }
    hash.to_string()
}

/// Same as [`normalize_nar_hash`] but operates on `Option<String>`,
/// returning `None` unchanged.
pub fn normalize_nar_hash_opt(hash: Option<String>) -> Option<String> {
    hash.map(|h| normalize_nar_hash(&h))
}

/// Strips a recognised algorithm prefix (`sha256:` or `blake3:`) from a
/// canonical hash, returning the bare nix32-encoded digest used in narinfo
/// `URL:` slugs. Returns the input unchanged if no recognised prefix is
/// found.
pub fn strip_hash_algo(hash: &str) -> &str {
    for algo in HASH_ALGOS {
        let prefix = format!("{algo}:");
        if let Some(rest) = hash.strip_prefix(&prefix) {
            return rest;
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY_SHA256: [u8; 32] = [
        0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9,
        0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52,
        0xb8, 0x55,
    ];
    const EMPTY_SHA256_NIX32: &str = "0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73";
    const EMPTY_SHA256_HEX: &str =
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    // BLAKE3 digest of "abc" — cross-checked against NixOS/nix PR #12379.
    const BLAKE3_ABC: [u8; 32] = [
        0x64, 0x37, 0xb3, 0xac, 0x38, 0x46, 0x51, 0x33, 0xff, 0xb6, 0x3b, 0x75, 0x27, 0x3a, 0x8d,
        0xb5, 0x48, 0xc5, 0x58, 0x46, 0x5d, 0x79, 0xdb, 0x03, 0xfd, 0x35, 0x9c, 0x6c, 0xd5, 0xbd,
        0x9d, 0x85,
    ];
    const BLAKE3_ABC_NIX32: &str = "11cxppanr71mzl1xnyax8rccaj5milx2fx9vnvzk6la672nb6dv4";
    const BLAKE3_ABC_HEX: &str =
        "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85";

    #[test]
    fn nix32_encode_zeros_all_zero_chars() {
        let result = nix32_encode(&[0u8; 32]);
        assert_eq!(result.len(), 52);
        assert!(result.chars().all(|c| c == '0'));
    }

    #[test]
    fn nix32_encode_known_vector() {
        assert_eq!(nix32_encode(&EMPTY_SHA256), EMPTY_SHA256_NIX32);
    }

    #[test]
    fn nix32_encode_blake3_known_vector() {
        assert_eq!(nix32_encode(&BLAKE3_ABC), BLAKE3_ABC_NIX32);
    }

    #[test]
    fn normalize_from_sri() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(EMPTY_SHA256);
        let sri = format!("sha256-{b64}");
        assert_eq!(
            normalize_nar_hash(&sri),
            format!("sha256:{EMPTY_SHA256_NIX32}")
        );
    }

    #[test]
    fn normalize_blake3_from_sri() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(BLAKE3_ABC);
        let sri = format!("blake3-{b64}");
        assert_eq!(
            normalize_nar_hash(&sri),
            format!("blake3:{BLAKE3_ABC_NIX32}")
        );
    }

    #[test]
    fn normalize_from_prefixed_hex() {
        let input = format!("sha256:{EMPTY_SHA256_HEX}");
        assert_eq!(
            normalize_nar_hash(&input),
            format!("sha256:{EMPTY_SHA256_NIX32}")
        );
    }

    #[test]
    fn normalize_blake3_from_prefixed_hex() {
        let input = format!("blake3:{BLAKE3_ABC_HEX}");
        assert_eq!(
            normalize_nar_hash(&input),
            format!("blake3:{BLAKE3_ABC_NIX32}")
        );
    }

    #[test]
    fn normalize_already_nix32_passthrough() {
        let input = format!("sha256:{EMPTY_SHA256_NIX32}");
        assert_eq!(normalize_nar_hash(&input), input);
    }

    #[test]
    fn normalize_blake3_already_nix32_passthrough() {
        let input = format!("blake3:{BLAKE3_ABC_NIX32}");
        assert_eq!(normalize_nar_hash(&input), input);
    }

    #[test]
    fn normalize_from_bare_hex() {
        assert_eq!(
            normalize_nar_hash(EMPTY_SHA256_HEX),
            format!("sha256:{EMPTY_SHA256_NIX32}")
        );
    }

    #[test]
    fn normalize_rejects_wrong_length_hex() {
        let short = &EMPTY_SHA256_HEX[..63];
        assert_eq!(normalize_nar_hash(short), short);
        let long = format!("{EMPTY_SHA256_HEX}a");
        assert_eq!(normalize_nar_hash(&long), long);
    }

    #[test]
    fn normalize_rejects_prefixed_non_64_hex() {
        let input = "sha256:abc";
        assert_eq!(normalize_nar_hash(input), input);
        let input = "blake3:abc";
        assert_eq!(normalize_nar_hash(input), input);
    }

    #[test]
    fn normalize_idempotent() {
        let once = normalize_nar_hash(EMPTY_SHA256_HEX);
        assert_eq!(normalize_nar_hash(&once), once);
        let once = normalize_nar_hash(BLAKE3_ABC_HEX);
        assert_eq!(normalize_nar_hash(&once), once);
    }

    #[test]
    fn strip_hash_algo_strips_known_prefixes() {
        assert_eq!(
            strip_hash_algo(&format!("sha256:{EMPTY_SHA256_NIX32}")),
            EMPTY_SHA256_NIX32
        );
        assert_eq!(
            strip_hash_algo(&format!("blake3:{BLAKE3_ABC_NIX32}")),
            BLAKE3_ABC_NIX32
        );
    }

    #[test]
    fn strip_hash_algo_passthrough_unknown() {
        assert_eq!(strip_hash_algo("md5:abc"), "md5:abc");
        assert_eq!(strip_hash_algo("noprefix"), "noprefix");
    }
}
