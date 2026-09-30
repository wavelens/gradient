/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::SourceError;
use base64::{Engine, engine::general_purpose};
use gradient_util::sync::Mutex;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Plaintexts by secret file and ciphertext. The password is stretched with
/// Argon2 on every decryption, tens of milliseconds each, and the same few keys
/// are decrypted on every dispatch, signature and update check.
static PLAINTEXTS: LazyLock<Mutex<HashMap<Ciphertext, Vec<u8>>>> = LazyLock::new(Default::default);

/// A ciphertext and the secret file it was encrypted under.
type Ciphertext = (String, Vec<u8>);

/// Decrypt `encrypted` with the key in `secret_file`, `None` when it does not
/// decrypt under that key.
pub(crate) fn decrypt_bytes(
    secret_file: &str,
    encrypted: Vec<u8>,
) -> Result<Option<Vec<u8>>, SourceError> {
    let key = (secret_file.to_owned(), encrypted);
    if let Some(plain) = PLAINTEXTS.lock().get(&key) {
        return Ok(Some(plain.clone()));
    }

    let secret = gradient_types::input::load_secret_bytes(secret_file).map_err(|e| {
        SourceError::FileRead {
            reason: e.to_string(),
        }
    })?;
    let Some(plain) = crypter::decrypt_with_password(secret.expose(), &key.1) else {
        return Ok(None);
    };
    PLAINTEXTS.lock().insert(key, plain.clone());
    Ok(Some(plain))
}

pub fn encrypt_secret(secret_file: &str, plaintext: &str) -> Result<String, SourceError> {
    let secret = gradient_types::input::load_secret_bytes(secret_file).map_err(|e| {
        SourceError::FileRead {
            reason: e.to_string(),
        }
    })?;
    let enc = crypter::encrypt_with_password(secret.expose(), plaintext.as_bytes())
        .ok_or(SourceError::CryptographicOperation)?;
    Ok(general_purpose::STANDARD.encode(enc))
}

pub fn decrypt_secret(secret_file: &str, blob_b64: &str) -> Result<String, SourceError> {
    let raw = general_purpose::STANDARD
        .decode(blob_b64.trim())
        .map_err(|_| SourceError::CryptographicOperation)?;
    let dec = decrypt_bytes(secret_file, raw)?.ok_or(SourceError::CryptographicOperation)?;
    String::from_utf8(dec).map_err(|_| SourceError::KeyUtf8Conversion)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_secret_file() -> (tempfile::NamedTempFile, String) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"test-secret-key-32-bytes-padding!").unwrap();
        f.flush().unwrap();
        let p = f.path().to_string_lossy().to_string();
        (f, p)
    }

    #[test]
    fn roundtrip() {
        let (_f, p) = temp_secret_file();
        let enc = encrypt_secret(&p, "GRADtoken123").unwrap();
        assert_ne!(enc, "GRADtoken123");
        assert_eq!(decrypt_secret(&p, &enc).unwrap(), "GRADtoken123");
    }

    #[test]
    fn a_decrypted_secret_is_not_decrypted_again() {
        let (f, p) = temp_secret_file();
        let enc = encrypt_secret(&p, "GRADonce").unwrap();
        assert_eq!(decrypt_secret(&p, &enc).unwrap(), "GRADonce");

        drop(f);
        assert_eq!(decrypt_secret(&p, &enc).unwrap(), "GRADonce");
    }

    #[test]
    fn decrypt_garbage_fails() {
        let (_f, p) = temp_secret_file();
        assert!(decrypt_secret(&p, "!!notbase64!!").is_err());
    }
}
