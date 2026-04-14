/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::SourceError;
use crate::types::*;
use anyhow::Result;
use base64::{Engine, engine::general_purpose};
use ed25519_compact::KeyPair;
use ssh_key::{
    Algorithm, LineEnding, PrivateKey, private::Ed25519Keypair, private::Ed25519PrivateKey,
    private::KeypairData, public::Ed25519PublicKey,
};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use tempfile::NamedTempFile;

pub fn write_key(private_key: String) -> Result<String, SourceError> {
    let mut temp_file = NamedTempFile::with_suffix(".key").map_err(|e| SourceError::FileRead {
        reason: e.to_string(),
    })?;

    let path = temp_file.path().to_string_lossy().to_string();

    fs::set_permissions(temp_file.path(), fs::Permissions::from_mode(0o600))
        .map_err(|_| SourceError::KeyFilePermissions { path: path.clone() })?;

    temp_file
        .write_all(private_key.as_bytes())
        .map_err(|_| SourceError::KeyFileWrite { path: path.clone() })?;

    temp_file
        .keep()
        .map_err(|_| SourceError::KeyFileWrite { path: path.clone() })?;

    Ok(path)
}

pub fn clear_key(path: String) -> Result<(), SourceError> {
    fs::remove_file(&path).map_err(|_| SourceError::KeyFileRemoval { path })?;
    Ok(())
}

pub fn generate_ssh_key(secret_file: String) -> Result<(String, String), SourceError> {
    let secret = crate::types::input::load_secret_bytes(&secret_file);

    let keypair = KeyPair::generate();

    let public_key_bytes: [u8; 32] = keypair
        .pk
        .as_slice()
        .try_into()
        .map_err(|_| SourceError::SshKeyGeneration)?;
    let private_key_bytes: [u8; 32] = keypair
        .sk
        .seed()
        .as_slice()
        .try_into()
        .map_err(|_| SourceError::SshKeyGeneration)?;

    let keypair_data = KeypairData::Ed25519(Ed25519Keypair {
        public: Ed25519PublicKey::try_from(&public_key_bytes[..])
            .map_err(|_| SourceError::SshKeyGeneration)?,
        private: Ed25519PrivateKey::from_bytes(&private_key_bytes),
    });
    let private_key =
        PrivateKey::new(keypair_data, "").map_err(|_| SourceError::SshKeyGeneration)?;

    let private_key_openssh = private_key
        .to_openssh(LineEnding::LF)
        .map_err(|_| SourceError::SshKeyGeneration)?
        .to_string();

    let public_key_parts = private_key
        .public_key()
        .to_openssh()
        .map_err(|_| SourceError::SshKeyGeneration)?
        .to_string();

    let public_key_data = public_key_parts
        .split_whitespace()
        .nth(1)
        .ok_or(SourceError::InvalidSshKey)?;

    let public_key_openssh = format!("{} {}", Algorithm::Ed25519.as_str(), public_key_data);

    let encrypted_private_key =
        crypter::encrypt_with_password(secret.expose(), &private_key_openssh)
            .ok_or(SourceError::CryptographicOperation)?;

    let encrypted_private_key = general_purpose::STANDARD.encode(&encrypted_private_key);

    Ok((encrypted_private_key, public_key_openssh))
}

pub fn decrypt_ssh_private_key(
    secret_file: String,
    organization: MOrganization,
    serve_url: &str,
) -> Result<(String, String), SourceError> {
    let secret = crate::types::input::load_secret_bytes(&secret_file);

    let encrypted_private_key = general_purpose::STANDARD
        .decode(organization.clone().private_key)
        .map_err(|e| SourceError::OrganizationKeyDecoding {
            org: organization.name.clone(),
            reason: format!("{}. The private key in the database appears to be corrupted or not properly base64-encoded.", e)
        })?;

    let decrypted_private_key = if let Some(p) =
        crypter::decrypt_with_password(secret.expose(), encrypted_private_key.clone())
    {
        String::from_utf8(p).map_err(|_| SourceError::KeyUtf8Conversion)?
    } else {
        tracing::warn!(
            "Failed to decrypt private key for organization '{}', attempting to decode as plaintext base64",
            organization.name
        );
        match String::from_utf8(encrypted_private_key) {
            Ok(plaintext) => {
                if plaintext.starts_with("-----BEGIN") {
                    tracing::warn!(
                        "Organization '{}' private key appears to be stored as plaintext base64",
                        organization.name
                    );
                    plaintext
                } else {
                    return Err(SourceError::KeyDecryption {
                        org: organization.name.clone(),
                    });
                }
            }
            Err(_) => {
                return Err(SourceError::KeyDecryption {
                    org: organization.name.clone(),
                });
            }
        }
    };

    let formatted_public_key = format_public_key(organization, serve_url);
    let decrypted_private_key = format!("{}\n", decrypted_private_key);

    Ok((decrypted_private_key, formatted_public_key))
}

pub fn format_public_key(organization: MOrganization, serve_url: &str) -> String {
    let hostname = serve_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(serve_url);
    format!(
        "{} {}-{}",
        organization.public_key, hostname, organization.name
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use uuid::Uuid;

    fn make_org(name: &str, public_key: &str) -> MOrganization {
        MOrganization {
            id: Uuid::nil(),
            name: name.to_string(),
            display_name: name.to_string(),
            description: String::new(),
            public_key: public_key.to_string(),
            private_key: String::new(),
            use_nix_store: false,
            public: false,
            created_by: Uuid::nil(),
            created_at: NaiveDateTime::default(),
            managed: false,
            github_installation_id: None,
            forge_webhook_secret: None,
        }
    }

    #[test]
    fn format_public_key_strips_https() {
        let org = make_org("myorg", "ssh-ed25519 AAAA");
        let result = format_public_key(org, "https://example.com");
        assert_eq!(result, "ssh-ed25519 AAAA example.com-myorg");
    }

    #[test]
    fn format_public_key_strips_path() {
        let org = make_org("myorg", "ssh-ed25519 AAAA");
        let result = format_public_key(org, "https://example.com/api/v1");
        assert_eq!(result, "ssh-ed25519 AAAA example.com-myorg");
    }

    #[test]
    fn format_public_key_format() {
        let org = make_org("wavelens", "ssh-ed25519 BBBB");
        let result = format_public_key(org, "https://gradient.wavelens.io");
        assert_eq!(result, "ssh-ed25519 BBBB gradient.wavelens.io-wavelens");
    }
}
