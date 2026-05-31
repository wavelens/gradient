/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::SourceError;
use base64::{Engine, engine::general_purpose};

pub fn encrypt_secret(secret_file: &str, plaintext: &str) -> Result<String, SourceError> {
    let secret = crate::types::input::load_secret_bytes(secret_file)
        .map_err(|e| SourceError::FileRead { reason: e.to_string() })?;
    let enc = crypter::encrypt_with_password(secret.expose(), plaintext.as_bytes())
        .ok_or(SourceError::CryptographicOperation)?;
    Ok(general_purpose::STANDARD.encode(enc))
}

pub fn decrypt_secret(secret_file: &str, blob_b64: &str) -> Result<String, SourceError> {
    let secret = crate::types::input::load_secret_bytes(secret_file)
        .map_err(|e| SourceError::FileRead { reason: e.to_string() })?;
    let raw = general_purpose::STANDARD
        .decode(blob_b64.trim())
        .map_err(|_| SourceError::CryptographicOperation)?;
    let dec = crypter::decrypt_with_password(secret.expose(), raw)
        .ok_or(SourceError::CryptographicOperation)?;
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
    fn decrypt_garbage_fails() {
        let (_f, p) = temp_secret_file();
        assert!(decrypt_secret(&p, "!!notbase64!!").is_err());
    }
}
