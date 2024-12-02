/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */

use std::sync::Arc;
use pkcs8::{EncodePrivateKey, EncodePublicKey};
use base64::{engine::general_purpose, Engine as _};
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;

use super::types::*;

pub async fn check_project_updates(state: Arc<ServerState>, project: &MProject) -> bool {
    println!("Checking for updates on project: {}", project.id);
    // TODO: dummy
    true
}

pub fn generate_ssh_key(state: Arc<ServerState>) -> Result<(String, String), String> {
    let mut csprng = OsRng;
    let signing_key = SigningKey::generate(&mut csprng);

    let private_key_pem = signing_key
        .to_pkcs8_pem(pkcs8::LineEnding::LF)
        .unwrap_or_else(|_| panic!("Failed to encode private key to PEM"))
        .to_string();

    let verifying_key = signing_key.verifying_key();
    let public_key_pem = verifying_key
        .to_public_key_pem(pkcs8::LineEnding::LF)
        .unwrap_or_else(|_| panic!("Failed to encode public key to PEM"))
        .lines()
        .nth(1)
        .unwrap()
        .to_string();

    let secret = general_purpose::STANDARD.decode(&state.cli.crypt_secret).unwrap();
    let encrypted_private_key = crypter::encrypt(secret, &private_key_pem).unwrap();
    let encrypted_private_key = general_purpose::STANDARD.encode(&encrypted_private_key);

    Ok((encrypted_private_key, public_key_pem))
}

pub fn decrypt_ssh_private_key(state: Arc<ServerState>, organization: MOrganization) -> Result<(String, String), String> {
    let secret = general_purpose::STANDARD.decode(&state.cli.crypt_secret).unwrap();
    let encrypted_private_key = general_purpose::STANDARD.decode(organization.private_key).unwrap();
    let decrypted_private_key = crypter::decrypt(secret, encrypted_private_key).unwrap();
    let decrypted_private_key = String::from_utf8(decrypted_private_key).unwrap();
    let formatted_public_key = format!("ed25519 {} {}", organization.public_key, organization.id);

    Ok((decrypted_private_key, formatted_public_key))
}

