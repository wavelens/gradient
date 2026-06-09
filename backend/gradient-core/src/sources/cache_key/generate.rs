/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::sources::SourceError;
use base64::{Engine, engine::general_purpose};
use ed25519_compact::KeyPair;

/// Returns `(encrypted_private_key, public_key_b64)`.
/// The private key is the full 64-byte ed25519 keypair encrypted and base64-encoded.
/// The public key is the last 32 bytes of the keypair, base64-encoded in plaintext.
pub fn generate_signing_key(secret_file: &str) -> Result<(String, String), SourceError> {
    let secret =
        crate::types::input::load_secret_bytes(secret_file).map_err(|e| SourceError::FileRead {
            reason: e.to_string(),
        })?;

    let keypair = KeyPair::generate();
    // Base64-encode the full 64-byte keypair (seed || public key)
    let key_b64 = general_purpose::STANDARD.encode(*keypair);
    // Derive the standalone public key (last 32 bytes)
    let public_key_b64 = general_purpose::STANDARD.encode(*keypair.pk);

    let encrypted_private_key = crypter::encrypt_with_password(secret.expose(), key_b64.as_bytes())
        .ok_or(SourceError::CryptographicOperation)?;

    Ok((
        general_purpose::STANDARD.encode(&encrypted_private_key),
        public_key_b64,
    ))
}
