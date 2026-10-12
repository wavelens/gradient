/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::DynError;
use super::StateApplicator;
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose};
use gradient_types::input::load_secret_bytes;
use ssh_key::PrivateKey;
use std::fs;
use std::path::Path;

pub(crate) fn read_credential(
    credentials_dir: &Path,
    kind: &str,
    name: &str,
    suffix: &str,
    label: &str,
) -> Result<(String, String), DynError> {
    let path = format!(
        "{}/gradient_{}_{}_{}",
        credentials_dir.display(),
        kind,
        name,
        suffix
    );
    let contents = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {} {}: {}", label, path, e))?;
    Ok((contents, path))
}

/// The file must hold an argon2 PHC hash. The plaintext password is never stored.
pub(crate) fn parse_password_phc(contents: &str, path: &str) -> Result<String, DynError> {
    let phc = contents.trim().to_string();
    if !phc.starts_with("$argon2") {
        return Err(format!(
            "Password file {} does not contain an argon2 PHC hash (expected to start with `$argon2`). \
             Generate one with `gradient hash` or `argon2 ... -id -e`.",
            path
        )
        .into());
    }
    Ok(phc)
}

pub(crate) fn parse_api_key_hash(contents: &str, path: &str) -> Result<String, DynError> {
    let v = contents.trim();
    if v.len() != 64 || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "API key file {} must contain a lowercase 64-char hex SHA-256 hash of the token \
             (e.g. `printf %s \"$TOKEN\" | sha256sum | cut -d' ' -f1`).",
            path
        )
        .into());
    }
    Ok(v.to_ascii_lowercase())
}

pub(crate) fn derive_public_key(private_key: &str) -> Result<String> {
    let private_key = PrivateKey::from_openssh(private_key).context("parse private key")?;

    let public_key = private_key
        .public_key()
        .to_openssh()
        .context("derive public key")?;

    let key_parts: Vec<&str> = public_key.split_whitespace().collect();
    let cleaned_key = if key_parts.len() >= 2 {
        format!("{} {}", key_parts[0], key_parts[1])
    } else {
        public_key.to_string()
    };

    Ok(cleaned_key)
}

impl<'a> StateApplicator<'a> {
    pub(crate) fn encrypt_to_b64(&self, plain: &str, what: &str) -> Result<String, DynError> {
        let secret = load_secret_bytes(self.crypt_secret_file)
            .map_err(|e| format!("Failed to load crypt secret: {}", e))?;
        let bytes = crypter::encrypt_with_password(secret.expose(), plain)
            .ok_or_else(|| format!("Failed to encrypt {}", what))?;
        Ok(general_purpose::STANDARD.encode(&bytes))
    }
}

#[cfg(test)]
mod password_phc_tests {
    use super::parse_password_phc;

    #[test]
    fn accepts_argon2id_phc_hashes() {
        const PLAIN: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$dGVzdA";
        for h in [
            "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$abcdefghijklmnopqrstuvwxyz0123456789ABCD",
            PLAIN,
        ] {
            assert_eq!(parse_password_phc(h, "/tmp/p").unwrap(), h);
        }
        assert_eq!(
            parse_password_phc(&format!("{PLAIN}\n  \n"), "/tmp/p").unwrap(),
            PLAIN
        );
    }

    #[test]
    fn rejects_anything_but_an_argon2_phc_hash() {
        for input in ["hunter2\n", "$pbkdf2-sha256$i=600000$c2FsdA$aGFzaA"] {
            let err = parse_password_phc(input, "/tmp/p").unwrap_err();
            assert!(err.to_string().contains("argon2"), "{input}: {err}");
        }
    }
}

#[cfg(test)]
mod api_key_hash_tests {
    use super::parse_api_key_hash;

    const VALID: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn accepts_and_normalizes_a_sha256_hex_hash() {
        assert_eq!(parse_api_key_hash(VALID, "/tmp/k").unwrap(), VALID);
        assert_eq!(
            parse_api_key_hash(&format!("{VALID}\n"), "/tmp/k").unwrap(),
            VALID
        );
        assert_eq!(
            parse_api_key_hash(&VALID.to_ascii_uppercase(), "/tmp/k").unwrap(),
            VALID
        );
    }

    #[test]
    fn rejects_anything_but_64_hex_chars() {
        for input in [
            "notahashbutaverylongstring".to_string(),
            "deadbeef".to_string(),
            "z".repeat(64),
        ] {
            let err = parse_api_key_hash(&input, "/tmp/k").unwrap_err();
            assert!(err.to_string().contains("SHA-256"), "{input}");
        }
    }
}

#[cfg(test)]
mod helper_tests {
    use super::read_credential;
    use crate::provisioning::lookup_id;
    use std::collections::HashMap;
    use std::path::Path;
    use uuid::Uuid;

    #[test]
    fn lookups_and_reads_name_the_missing_thing() {
        let m: HashMap<String, Uuid> = HashMap::new();
        let err = lookup_id(&m, "ghost", "User").unwrap_err().to_string();
        assert!(err.contains("User") && err.contains("ghost"), "{err}");

        let dir = Path::new("/nonexistent-credentials");
        let err = read_credential(dir, "user", "alice", "password", "password file")
            .unwrap_err()
            .to_string();
        assert!(err.contains("password file"), "{err}");
        assert!(
            err.contains("/nonexistent-credentials/gradient_user_alice_password"),
            "{err}"
        );
    }
}
