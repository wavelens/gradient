/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::context::CiContext;
use anyhow::{Context, Result, anyhow};
use gradient_types::input::load_secret_bytes;

pub fn encrypt_action_secret(plaintext: &str, crypt_key: &[u8]) -> Result<String> {
    crate::action_crypto::encrypt(plaintext, crypt_key)
}

pub fn decrypt_action_secret(ciphertext: &str, crypt_key: &[u8]) -> Result<String> {
    crate::action_crypto::decrypt(ciphertext, crypt_key)
}

pub fn encrypt_secret_with_file(crypt_secret_file: &str, plaintext: &str) -> Result<String> {
    let key =
        load_secret_bytes(crypt_secret_file).map_err(|e| anyhow!("loading crypt key: {}", e))?;
    encrypt_action_secret(plaintext, key.expose())
}

pub fn decrypt_secret_with_file(
    crypt_secret_file: &str,
    ciphertext: &str,
) -> Result<gradient_types::SecretString> {
    let key =
        load_secret_bytes(crypt_secret_file).map_err(|e| anyhow!("loading crypt key: {}", e))?;
    decrypt_action_secret(ciphertext, key.expose()).map(gradient_types::SecretString::new)
}

pub(crate) fn decrypt_with_server_key(ctx: &CiContext, ciphertext: &str) -> Result<String> {
    let key = load_secret_bytes(&ctx.db.config.secrets.crypt_file).context("loading crypt key")?;
    decrypt_action_secret(ciphertext, key.expose())
}
