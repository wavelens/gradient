/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::SourceError;
use base64::{Engine, engine::general_purpose};
use ed25519_compact::KeyPair;

pub fn generate_signing_key(secret_file: &str) -> Result<(String, String), SourceError> {
    let secret = gradient_types::input::load_secret_bytes(secret_file).map_err(|e| {
        SourceError::FileRead {
            reason: e.to_string(),
        }
    })?;

    let keypair = KeyPair::generate();
    let key_b64 = general_purpose::STANDARD.encode(*keypair);
    let public_key_b64 = general_purpose::STANDARD.encode(*keypair.pk);

    let encrypted_private_key = crypter::encrypt_with_password(secret.expose(), key_b64.as_bytes())
        .ok_or(SourceError::CryptographicOperation)?;

    Ok((
        general_purpose::STANDARD.encode(&encrypted_private_key),
        public_key_b64,
    ))
}
