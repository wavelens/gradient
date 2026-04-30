/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Canonical encoding helpers for SHA-256 hashes used in narinfo metadata.
//!
//! All on-disk DB columns that hold a `file_hash` or `nar_hash` use the
//! `sha256:<nix32>` form so that lookups against the URL hash extracted from
//! a narinfo `URL:` field match without re-encoding. Workers may send hex,
//! SRI, or already-encoded nix32 over the wire — pass any incoming value
//! through [`normalize_nar_hash`] before persisting.

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

/// Converts any SHA-256 hash representation (SRI `sha256-{base64}`, nix32
/// `sha256:{nix32}`, prefixed hex `sha256:{hex}`, or bare hex) to the
/// canonical `sha256:{nix32}` form.
///
/// If the input matches none of the recognised formats it is returned
/// unchanged, preserving caller intent for already-canonical or sentinel
/// values.
pub fn normalize_nar_hash(hash: &str) -> String {
    if let Some(b64) = hash.strip_prefix("sha256-")
        && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64)
    {
        return format!("sha256:{}", nix32_encode(&bytes));
    }
    if let Some(rest) = hash.strip_prefix("sha256:") {
        if rest.len() == 64
            && rest.chars().all(|c| c.is_ascii_hexdigit())
            && let Ok(bytes) = (0..32)
                .map(|i| u8::from_str_radix(&rest[i * 2..i * 2 + 2], 16))
                .collect::<Result<Vec<u8>, _>>()
        {
            return format!("sha256:{}", nix32_encode(&bytes));
        }
        return hash.to_string();
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
    fn normalize_from_sri() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(EMPTY_SHA256);
        let sri = format!("sha256-{b64}");
        assert_eq!(normalize_nar_hash(&sri), format!("sha256:{EMPTY_SHA256_NIX32}"));
    }

    #[test]
    fn normalize_from_prefixed_hex() {
        let input = format!("sha256:{EMPTY_SHA256_HEX}");
        assert_eq!(normalize_nar_hash(&input), format!("sha256:{EMPTY_SHA256_NIX32}"));
    }

    #[test]
    fn normalize_already_nix32_passthrough() {
        let input = format!("sha256:{EMPTY_SHA256_NIX32}");
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
    }

    #[test]
    fn normalize_idempotent() {
        let once = normalize_nar_hash(EMPTY_SHA256_HEX);
        assert_eq!(normalize_nar_hash(&once), once);
    }
}
