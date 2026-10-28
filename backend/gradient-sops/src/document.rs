/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io::Read as _;
use std::path::Path;

use age::DecryptError;
use age::armor::ArmoredReader;
use gradient_secret::SecretBytes;
use yaml_serde::{Mapping, Value};
use zeroize::Zeroizing;

use crate::key::SopsKey;
use crate::mac::{DocumentWalk, verify_mac};
use crate::{SopsError, SopsSecrets};

const DATA_KEY_CAPACITY: usize = 64;

pub fn decrypt_file(path: &str, content: &[u8], key: &SopsKey) -> Result<SopsSecrets, SopsError> {
    let file = path.to_owned();
    check_format(&file)?;
    if contains(content, b"type:comment]") {
        return Err(SopsError::EncryptedComments { file });
    }

    let document = parse_document(&file, content)?;
    let metadata = document
        .get("sops")
        .and_then(Value::as_mapping)
        .ok_or_else(|| SopsError::parse(&file, "no sops metadata"))?;
    let data_key = data_key(&file, metadata, key)?;
    let mac_only_encrypted = metadata
        .get("mac_only_encrypted")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let walk = DocumentWalk::new(&file, data_key.expose(), mac_only_encrypted)?;
    let (secrets, digest) = walk.run(&document)?;
    verify_mac(&file, metadata, data_key.expose(), &digest)?;
    Ok(SopsSecrets(secrets))
}

fn check_format(file: &str) -> Result<(), SopsError> {
    match Path::new(file).extension().and_then(|e| e.to_str()) {
        Some("yaml" | "yml" | "json") => Ok(()),
        _ => Err(SopsError::UnsupportedFormat { file: file.into() }),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn parse_document(file: &str, content: &[u8]) -> Result<Mapping, SopsError> {
    match yaml_serde::from_slice::<Value>(content) {
        Ok(Value::Mapping(document)) => Ok(document),
        Ok(_) => Err(SopsError::parse(file, "the top level is not a mapping")),
        Err(e) => Err(SopsError::parse(file, e.to_string())),
    }
}

fn data_key(file: &str, metadata: &Mapping, key: &SopsKey) -> Result<SecretBytes, SopsError> {
    let stanzas = metadata
        .get("age")
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("enc").and_then(Value::as_str));

    for armored in stanzas {
        match open_age(armored, key) {
            Ok(data_key) => return Ok(data_key),
            Err(DecryptError::NoMatchingKeys) => continue,
            Err(e) => return Err(SopsError::parse(file, e.to_string())),
        }
    }

    Err(SopsError::NotARecipient { file: file.into() })
}

fn open_age(armored: &str, key: &SopsKey) -> Result<SecretBytes, DecryptError> {
    let decryptor = age::Decryptor::new_buffered(ArmoredReader::new(armored.as_bytes()))?;
    let mut reader = decryptor.decrypt(key.identities())?;
    let mut data_key = Zeroizing::new(Vec::with_capacity(DATA_KEY_CAPACITY));
    reader.read_to_end(&mut data_key)?;
    Ok(SecretBytes::from(data_key.as_slice()))
}
