/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Credential-file reading, secret encryption, and SSH key derivation.

use super::DynError;
use super::StateApplicator;
use crate::types::input::load_secret_bytes;
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose};
use ssh_key::PrivateKey;
use std::fs;

pub(crate) fn credentials_dir() -> String {
    std::env::var("GRADIENT_CREDENTIALS_DIR")
        .unwrap_or_else(|_| "/run/credentials/gradient-server".to_string())
}

/// Reads `${GRADIENT_CREDENTIALS_DIR}/gradient_${kind}_${name}_${suffix}` and
/// returns `(contents, path)`. The path is returned alongside so callers can
/// embed it in downstream validation errors.
pub(crate) fn read_credential(
    kind: &str,
    name: &str,
    suffix: &str,
    label: &str,
) -> Result<(String, String), DynError> {
    let path = format!(
        "{}/gradient_{}_{}_{}",
        credentials_dir(),
        kind,
        name,
        suffix
    );
    let contents = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {} {}: {}", label, path, e))?;
    Ok((contents, path))
}

/// Validate the contents of a user password credential file. The file must
/// contain an argon2 PHC hash (e.g. produced by `gradient-server hash` or the
/// `argon2 -id -e` CLI). The plaintext password is never stored - the server
/// only accepts the pre-hashed PHC string and passes it through to the DB.
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
    let private_key =
        PrivateKey::from_openssh(private_key).context("Failed to parse private key")?;

    let public_key = private_key
        .public_key()
        .to_openssh()
        .context("Failed to derive public key")?;

    let key_parts: Vec<&str> = public_key.split_whitespace().collect();
    let cleaned_key = if key_parts.len() >= 2 {
        format!("{} {}", key_parts[0], key_parts[1])
    } else {
        public_key.to_string()
    };

    Ok(cleaned_key)
}

impl<'a> StateApplicator<'a> {
    /// Encrypt `plain` with the configured crypt secret and return its
    /// base64-encoded form. `what` describes the secret for error messages.
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
    fn accepts_argon2id_phc_hash() {
        let h = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$abcdefghijklmnopqrstuvwxyz0123456789ABCD";
        let parsed = parse_password_phc(h, "/tmp/p").unwrap();
        assert_eq!(parsed, h);
    }

    #[test]
    fn trims_trailing_whitespace_and_newlines() {
        let h = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$dGVzdA";
        let with_ws = format!("{h}\n  \n");
        let parsed = parse_password_phc(&with_ws, "/tmp/p").unwrap();
        assert_eq!(parsed, h);
    }

    #[test]
    fn rejects_plaintext_password() {
        let err = parse_password_phc("hunter2\n", "/tmp/p").unwrap_err();
        assert!(err.to_string().contains("argon2 PHC hash"));
    }

    #[test]
    fn rejects_other_phc_algorithms() {
        let h = "$pbkdf2-sha256$i=600000$c2FsdA$aGFzaA";
        let err = parse_password_phc(h, "/tmp/p").unwrap_err();
        assert!(err.to_string().contains("argon2"));
    }
}

#[cfg(test)]
mod api_key_hash_tests {
    use super::parse_api_key_hash;

    const VALID: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn accepts_64_char_hex() {
        assert_eq!(parse_api_key_hash(VALID, "/tmp/k").unwrap(), VALID);
    }

    #[test]
    fn trims_trailing_whitespace() {
        let with_ws = format!("{VALID}\n");
        assert_eq!(parse_api_key_hash(&with_ws, "/tmp/k").unwrap(), VALID);
    }

    #[test]
    fn lowercases_uppercase_hex() {
        let upper = VALID.to_ascii_uppercase();
        assert_eq!(parse_api_key_hash(&upper, "/tmp/k").unwrap(), VALID);
    }

    #[test]
    fn rejects_plaintext_token() {
        let err = parse_api_key_hash("notahashbutaverylongstring", "/tmp/k").unwrap_err();
        assert!(err.to_string().contains("SHA-256"));
    }

    #[test]
    fn rejects_short_hex() {
        let err = parse_api_key_hash("deadbeef", "/tmp/k").unwrap_err();
        assert!(err.to_string().contains("SHA-256"));
    }

    #[test]
    fn rejects_non_hex_chars() {
        let bad = "z".repeat(64);
        let err = parse_api_key_hash(&bad, "/tmp/k").unwrap_err();
        assert!(err.to_string().contains("SHA-256"));
    }
}

#[cfg(test)]
mod helper_tests {
    use super::{credentials_dir, read_credential};
    use crate::state::provisioning::lookup_id;
    use std::collections::HashMap;
    use uuid::Uuid;

    #[test]
    fn lookup_id_returns_id_when_present() {
        let id = Uuid::now_v7();
        let mut m = HashMap::new();
        m.insert("alice".to_string(), id);
        assert_eq!(lookup_id(&m, "alice", "User").unwrap(), id);
    }

    #[test]
    fn lookup_id_errors_with_kind_and_name() {
        let m: HashMap<String, Uuid> = HashMap::new();
        let err = lookup_id(&m, "ghost", "User").unwrap_err();
        let s = err.to_string();
        assert!(s.contains("User"));
        assert!(s.contains("ghost"));
    }

    #[test]
    fn read_credential_default_dir_when_env_unset() {
        // Without GRADIENT_CREDENTIALS_DIR set, credentials_dir() returns the
        // built-in systemd-credentials path. The read fails (no such file),
        // so we just verify the error embeds the expected suffix and label.
        // We don't assert on the env var (other tests run in parallel and
        // may set it concurrently).
        let err = read_credential("user", "alice", "password", "password file").unwrap_err();
        let s = err.to_string();
        assert!(s.contains("password file"));
        assert!(s.contains("gradient_user_alice_password"));
    }

    #[test]
    fn credentials_dir_returns_nonempty() {
        // We can't assert the exact value without racing on env state, but it
        // must always be a non-empty path so format!() composes a valid path.
        assert!(!credentials_dir().is_empty());
    }
}
