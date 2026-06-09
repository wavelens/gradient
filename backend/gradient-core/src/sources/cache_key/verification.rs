/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::narinfo;
use base64::{Engine, engine::general_purpose};
use ed25519_compact::{PublicKey, Signature};

/// Verifies that `narinfo_body` carries at least one `Sig:` line signed by the
/// holder of `public_key`.
///
/// `public_key` is the standard Nix narinfo format: `{name}:{base64-32-byte-pubkey}`.
///
/// The fingerprint signed by Nix caches is:
///
/// ```text
/// 1;{StorePath};{NarHash};{NarSize};{sorted-/nix/store/-prefixed-refs-comma-joined}
/// ```
///
/// Returns `true` if any `Sig: {name}:{sig}` line with a matching key name
/// verifies under the given public key. Returns `false` if the body is
/// malformed, the public key can't be decoded, or no matching signature
/// verifies - the caller is expected to treat `false` as "upstream lacks
/// a trusted signature for this path".
pub fn verify_narinfo_signature(public_key: &str, narinfo_body: &str) -> bool {
    let Some((key_name, pub_b64)) = public_key.split_once(':') else {
        return false;
    };
    let Ok(pub_bytes) = general_purpose::STANDARD.decode(pub_b64.trim()) else {
        return false;
    };
    let Ok(pubkey) = PublicKey::from_slice(&pub_bytes) else {
        return false;
    };

    let mut store_path: Option<&str> = None;
    let mut nar_hash: Option<&str> = None;
    let mut nar_size: Option<&str> = None;
    let mut references: Vec<&str> = Vec::new();
    let mut sigs: Vec<&str> = Vec::new();

    for line in narinfo_body.lines() {
        if let Some(v) = line.strip_prefix("StorePath: ") {
            store_path = Some(v.trim());
        } else if let Some(v) = line.strip_prefix("NarHash: ") {
            nar_hash = Some(v.trim());
        } else if let Some(v) = line.strip_prefix("NarSize: ") {
            nar_size = Some(v.trim());
        } else if let Some(v) = line.strip_prefix("References: ") {
            references = v.split_whitespace().collect();
        } else if let Some(v) = line.strip_prefix("Sig: ") {
            sigs.push(v.trim());
        }
    }

    let (Some(store_path), Some(nar_hash), Some(nar_size)) = (store_path, nar_hash, nar_size)
    else {
        return false;
    };

    let fingerprint = narinfo::fingerprint(store_path, nar_hash, nar_size, references);

    for sig_token in sigs {
        let Some((sig_name, sig_b64)) = sig_token.split_once(':') else {
            continue;
        };
        if sig_name != key_name {
            continue;
        }
        let Ok(sig_bytes) = general_purpose::STANDARD.decode(sig_b64) else {
            continue;
        };
        let Ok(sig) = Signature::from_slice(&sig_bytes) else {
            continue;
        };
        if pubkey.verify(fingerprint.as_bytes(), &sig).is_ok() {
            return true;
        }
    }

    false
}
