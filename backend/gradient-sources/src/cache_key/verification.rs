/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::narinfo;
use base64::{Engine, engine::general_purpose};
use ed25519_compact::{PublicKey, Signature};
use gradient_types::NixPathInfo;

pub fn verify_narinfo_signature(public_key: &str, path_info: &NixPathInfo) -> bool {
    let Some((key_name, pub_b64)) = public_key.split_once(':') else {
        return false;
    };
    let Ok(pub_bytes) = general_purpose::STANDARD.decode(pub_b64.trim()) else {
        return false;
    };
    let Ok(pubkey) = PublicKey::from_slice(&pub_bytes) else {
        return false;
    };

    let fingerprint = narinfo::narinfo_signing_fingerprint(path_info);

    path_info.signature_lines.iter().any(|sig_token| {
        let Some((sig_name, sig_b64)) = sig_token.split_once(':') else {
            return false;
        };
        if sig_name != key_name {
            return false;
        }
        let Ok(sig_bytes) = general_purpose::STANDARD.decode(sig_b64) else {
            return false;
        };
        Signature::from_slice(&sig_bytes)
            .is_ok_and(|sig| pubkey.verify(fingerprint.as_bytes(), &sig).is_ok())
    })
}
