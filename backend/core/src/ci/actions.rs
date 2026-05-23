/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Project Actions dispatch and execution.

use anyhow::Result;

pub fn encrypt_action_secret(plaintext: &str, crypt_key: &[u8]) -> Result<String> {
    super::action_crypto::encrypt(plaintext, crypt_key)
}

pub fn decrypt_action_secret(ciphertext: &str, crypt_key: &[u8]) -> Result<String> {
    super::action_crypto::decrypt(ciphertext, crypt_key)
}
