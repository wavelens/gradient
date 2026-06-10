/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Reversible AES-256-GCM encryption for action secrets (Send Web Request bearer tokens,
//! and the legacy webhook HMAC secrets while that surface is being removed).

use anyhow::Result;
use base64::{Engine, engine::general_purpose};

pub fn encrypt(plaintext: &str, crypt_key: &[u8]) -> Result<String> {
    let ciphertext = crypter::encrypt_with_password(crypt_key, plaintext.as_bytes())
        .ok_or_else(|| anyhow::anyhow!("Encryption failed"))?;
    Ok(general_purpose::STANDARD.encode(ciphertext))
}

pub fn decrypt(ciphertext: &str, crypt_key: &[u8]) -> Result<String> {
    let raw = general_purpose::STANDARD
        .decode(ciphertext)
        .map_err(|e| anyhow::anyhow!("Base64 decode error: {}", e))?;
    let plaintext = crypter::decrypt_with_password(crypt_key, raw)
        .ok_or_else(|| anyhow::anyhow!("Decryption failed"))?;
    String::from_utf8(plaintext).map_err(|e| anyhow::anyhow!("UTF-8 decode error: {}", e))
}
