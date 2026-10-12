/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! BLAKE3 is still accepted on the read path. Rows uploaded under the old BLAKE3 default (issue
//! #132) must keep resolving. DB columns are holding `{algo}:{nix32}` values. The narinfo URL hash
//! slug is then matching the column verbatim.

use base64::Engine as _;

pub const NIX32_CHARS: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";

pub fn is_nix32_hash(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| NIX32_CHARS.contains(&b))
}

pub fn nix32_encode(bytes: &[u8]) -> String {
    let len = (bytes.len() * 8 - 1) / 5 + 1;
    let mut out = String::with_capacity(len);
    for n in (0..len).rev() {
        let b = n * 5;
        let i = b / 8;
        let j = b % 8;
        let byte0 = bytes.get(i).copied().unwrap_or(0) as u32;
        let byte1 = bytes.get(i + 1).copied().unwrap_or(0) as u32;
        let c = ((byte0 >> j) | (byte1 << (8 - j))) & 0x1f;
        out.push(NIX32_CHARS[c as usize] as char);
    }
    out
}

const HASH_ALGOS: &[&str] = &["sha256", "blake3"];

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

pub fn normalize_nar_hash_opt(hash: Option<String>) -> Option<String> {
    hash.map(|h| normalize_nar_hash(&h))
}

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

    // BLAKE3 digest of "abc" is cross-checked against NixOS/nix PR #12379.
    const BLAKE3_ABC: [u8; 32] = [
        0x64, 0x37, 0xb3, 0xac, 0x38, 0x46, 0x51, 0x33, 0xff, 0xb6, 0x3b, 0x75, 0x27, 0x3a, 0x8d,
        0xb5, 0x48, 0xc5, 0x58, 0x46, 0x5d, 0x79, 0xdb, 0x03, 0xfd, 0x35, 0x9c, 0x6c, 0xd5, 0xbd,
        0x9d, 0x85,
    ];
    const BLAKE3_ABC_NIX32: &str = "11cxppanr71mzl1xnyax8rccaj5milx2fx9vnvzk6la672nb6dv4";
    const BLAKE3_ABC_HEX: &str = "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85";

    #[test]
    fn nix32_encode_known_digest_matches_vector() {
        let zeros = "0".repeat(52);
        let cases: &[(&[u8], &str)] = &[
            (&[0u8; 32], &zeros),
            (&EMPTY_SHA256, EMPTY_SHA256_NIX32),
            (&BLAKE3_ABC, BLAKE3_ABC_NIX32),
        ];
        for (bytes, expected) in cases {
            assert_eq!(nix32_encode(bytes), *expected);
        }
    }

    #[test]
    fn normalize_nar_hash_by_input_format_yields_expected_idempotently() {
        let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let sha_nix32 = format!("sha256:{EMPTY_SHA256_NIX32}");
        let blake_nix32 = format!("blake3:{BLAKE3_ABC_NIX32}");
        let long_hex = format!("{EMPTY_SHA256_HEX}a");
        let cases: Vec<(String, String)> = vec![
            (format!("sha256-{}", b64(&EMPTY_SHA256)), sha_nix32.clone()),
            (format!("blake3-{}", b64(&BLAKE3_ABC)), blake_nix32.clone()),
            (format!("sha256:{EMPTY_SHA256_HEX}"), sha_nix32.clone()),
            (format!("blake3:{BLAKE3_ABC_HEX}"), blake_nix32.clone()),
            (sha_nix32.clone(), sha_nix32.clone()),
            (blake_nix32.clone(), blake_nix32.clone()),
            (EMPTY_SHA256_HEX.into(), sha_nix32),
            (BLAKE3_ABC_HEX.into(), format!("sha256:{BLAKE3_ABC_NIX32}")),
            (EMPTY_SHA256_HEX[..63].into(), EMPTY_SHA256_HEX[..63].into()),
            (long_hex.clone(), long_hex),
            ("sha256:abc".into(), "sha256:abc".into()),
            ("blake3:abc".into(), "blake3:abc".into()),
        ];
        for (input, expected) in &cases {
            let once = normalize_nar_hash(input);
            assert_eq!(&once, expected, "input {input}");
            assert_eq!(normalize_nar_hash(&once), once, "input {input}");
        }
    }

    #[test]
    fn strip_hash_algo_by_prefix_strips_only_known() {
        let sha = format!("sha256:{EMPTY_SHA256_NIX32}");
        let blake = format!("blake3:{BLAKE3_ABC_NIX32}");
        let cases: &[(&str, &str)] = &[
            (&sha, EMPTY_SHA256_NIX32),
            (&blake, BLAKE3_ABC_NIX32),
            ("md5:abc", "md5:abc"),
            ("noprefix", "noprefix"),
        ];
        for (input, expected) in cases {
            assert_eq!(strip_hash_algo(input), *expected);
        }
    }
}
