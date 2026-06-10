/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::SourceError;
use gradient_types::*;
use anyhow::Result;
use base64::{Engine, engine::general_purpose};
use ed25519_compact::KeyPair;
use ::ssh_key::{
    Algorithm, LineEnding, PrivateKey, private::Ed25519Keypair, private::Ed25519PrivateKey,
    private::KeypairData, public::Ed25519PublicKey,
};
pub fn generate_ssh_key(secret_file: &str) -> Result<(String, String), SourceError> {
    let secret =
        gradient_types::input::load_secret_bytes(secret_file).map_err(|e| SourceError::FileRead {
            reason: e.to_string(),
        })?;

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
    secret_file: &str,
    organization: MOrganization,
    serve_url: &str,
) -> Result<(String, String), SourceError> {
    let secret =
        gradient_types::input::load_secret_bytes(secret_file).map_err(|e| SourceError::FileRead {
            reason: e.to_string(),
        })?;

    let encrypted_private_key = general_purpose::STANDARD
        .decode(organization.clone().private_key)
        .map_err(|e| SourceError::OrganizationKeyDecoding {
            org: organization.name.clone(),
            reason: format!("{}. The private key in the database appears to be corrupted or not properly base64-encoded.", e)
        })?;

    let decrypted_private_key =
        match crypter::decrypt_with_password(secret.expose(), encrypted_private_key) {
            Some(p) => String::from_utf8(p).map_err(|_| SourceError::KeyUtf8Conversion)?,
            None => {
                return Err(SourceError::KeyDecryption {
                    org: organization.name.clone(),
                });
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

    fn make_org(name: &str, public_key: &str) -> MOrganization {
        MOrganization {
            id: gradient_types::ids::OrganizationId::nil(),
            name: name.to_string(),
            display_name: name.to_string(),
            description: String::new(),
            public_key: public_key.to_string(),
            private_key: String::new(),
            public: false,
            hide_build_requests: false,
            created_by: gradient_types::ids::UserId::nil(),
            created_at: NaiveDateTime::default(),
            managed: false,
            github_installation_id: None,
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

    #[test]
    fn format_public_key_strips_http() {
        let org = make_org("myorg", "ssh-ed25519 AAAA");
        let result = format_public_key(org, "http://example.com");
        assert_eq!(result, "ssh-ed25519 AAAA example.com-myorg");
    }

    #[test]
    fn format_public_key_fallback_without_scheme() {
        // When no scheme is present, the raw URL is used as the hostname
        // (no path stripping either, since there's nothing to strip).
        let org = make_org("myorg", "ssh-ed25519 AAAA");
        let result = format_public_key(org, "example.com");
        assert_eq!(result, "ssh-ed25519 AAAA example.com-myorg");
    }

    #[test]
    fn decrypt_ssh_key_corrupt_base64_fails() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"test-secret-key-32-bytes-padding!").unwrap();
        let path = f.path().to_string_lossy().to_string();
        let mut org = make_org("o", "ssh-ed25519 AAAA");
        org.private_key = "!!!not-base64!!!".to_string();
        let result = decrypt_ssh_private_key(&path, org, "https://example.com");
        assert!(matches!(
            result,
            Err(SourceError::OrganizationKeyDecoding { .. })
        ));
    }

    #[test]
    fn decrypt_ssh_key_plaintext_pem_rejected() {
        // Plaintext PEM stored in the column must NOT be accepted -
        // doing so would let anyone with DB write access bypass encryption.
        let mut f = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"test-secret-key-32-bytes-padding!").unwrap();
        let path = f.path().to_string_lossy().to_string();
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n-----END OPENSSH PRIVATE KEY-----";
        let mut org = make_org("o", "ssh-ed25519 AAAA");
        org.private_key = general_purpose::STANDARD.encode(pem);
        let result = decrypt_ssh_private_key(&path, org, "https://example.com");
        assert!(matches!(result, Err(SourceError::KeyDecryption { .. })));
    }

    #[test]
    fn decrypt_ssh_key_plaintext_non_pem_rejected() {
        // Valid base64 that decrypts to garbage AND is not a PEM must fail.
        let mut f = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"test-secret-key-32-bytes-padding!").unwrap();
        let path = f.path().to_string_lossy().to_string();
        let mut org = make_org("o", "ssh-ed25519 AAAA");
        org.private_key = general_purpose::STANDARD.encode(b"random garbage not a key");
        let result = decrypt_ssh_private_key(&path, org, "https://example.com");
        assert!(matches!(result, Err(SourceError::KeyDecryption { .. })));
    }

    #[test]
    fn generate_ssh_key_decrypts_to_openssh_pem() {
        // Round-trip: generated keys must decrypt to a PEM the loader accepts,
        // and the returned tuple must pair correctly.
        let mut f = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"test-secret-key-32-bytes-padding!").unwrap();
        let path = f.path().to_string_lossy().to_string();
        let (enc_priv, pub_openssh) = generate_ssh_key(&path).expect("generate failed");
        assert!(pub_openssh.starts_with("ssh-ed25519 "));

        let mut org = make_org("myorg", &pub_openssh);
        org.private_key = enc_priv;
        let (priv_pem, pub_formatted) =
            decrypt_ssh_private_key(&path, org, "https://example.com").expect("decrypt failed");
        assert!(priv_pem.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(pub_formatted.starts_with(&pub_openssh));
        assert!(pub_formatted.ends_with(" example.com-myorg"));
    }
}
