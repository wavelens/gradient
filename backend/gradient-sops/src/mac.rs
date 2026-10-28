/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;

use aes_gcm::aead::consts::U32;
use aes_gcm::aead::{Aead as _, KeyInit as _, Payload};
use aes_gcm::aes::Aes256;
use aes_gcm::{AesGcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest as _, Sha512};
use yaml_serde::{Mapping, Value};
use zeroize::Zeroizing;

use crate::SopsError;

type SopsCipher = AesGcm<Aes256, U32>;
type Secrets = BTreeMap<String, Zeroizing<Vec<u8>>>;

struct EncryptedValue {
    data: Vec<u8>,
    iv: Vec<u8>,
    tag: Vec<u8>,
    kind: String,
}

impl EncryptedValue {
    fn parse(file: &str, text: &str) -> Result<Self, SopsError> {
        let fields = text
            .strip_prefix("ENC[AES256_GCM,")
            .and_then(|rest| rest.strip_suffix(']'))
            .ok_or_else(|| SopsError::parse(file, "unsupported encrypted value"))?;
        let field = |name: &str| {
            fields
                .split(',')
                .find_map(|part| part.strip_prefix(name)?.strip_prefix(':'))
                .ok_or_else(|| SopsError::parse(file, format!("encrypted value without {name}")))
        };
        let bytes = |name: &str| {
            STANDARD
                .decode(field(name)?)
                .map_err(|e| SopsError::parse(file, e.to_string()))
        };

        Ok(Self {
            data: bytes("data")?,
            iv: bytes("iv")?,
            tag: bytes("tag")?,
            kind: field("type")?.to_owned(),
        })
    }

    fn open(
        &self,
        file: &str,
        cipher: &SopsCipher,
        aad: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, SopsError> {
        let nonce = <&Nonce<U32>>::try_from(self.iv.as_slice())
            .map_err(|_| SopsError::parse(file, "encrypted value with a wrong iv size"))?;
        let mut ciphertext = self.data.clone();
        ciphertext.extend_from_slice(&self.tag);
        cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &ciphertext,
                    aad,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| SopsError::MacMismatch { file: file.into() })
    }
}

pub(crate) struct DocumentWalk<'a> {
    file: &'a str,
    cipher: SopsCipher,
    mac_only_encrypted: bool,
    hasher: Sha512,
}

impl<'a> DocumentWalk<'a> {
    pub(crate) fn new(
        file: &'a str,
        data_key: &[u8],
        mac_only_encrypted: bool,
    ) -> Result<Self, SopsError> {
        Ok(Self {
            file,
            cipher: cipher(file, data_key)?,
            mac_only_encrypted,
            hasher: Sha512::new(),
        })
    }

    pub(crate) fn run(mut self, document: &Mapping) -> Result<(Secrets, String), SopsError> {
        let mut secrets = Secrets::new();
        for (key, value) in document {
            let name = key_text(key);
            if name == "sops" {
                continue;
            }

            let mut path = vec![name.clone()];
            if let Some(text) = self.walk(value, &mut path)? {
                secrets.insert(name, text);
            }
        }

        Ok((secrets, hex::encode_upper(self.hasher.finalize())))
    }

    fn walk(
        &mut self,
        value: &Value,
        path: &mut Vec<String>,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, SopsError> {
        match value {
            Value::Mapping(mapping) => {
                for (key, child) in mapping {
                    path.push(key_text(key));
                    self.walk(child, path)?;
                    path.pop();
                }
                Ok(None)
            }
            Value::Sequence(items) => {
                for item in items {
                    self.walk(item, path)?;
                }
                Ok(None)
            }
            Value::Tagged(tagged) => self.walk(&tagged.value, path),
            Value::String(text) if text.starts_with("ENC[") => self.decrypt_leaf(text, path),
            scalar => Ok(self.plain_leaf(scalar)),
        }
    }

    fn decrypt_leaf(
        &mut self,
        text: &str,
        path: &[String],
    ) -> Result<Option<Zeroizing<Vec<u8>>>, SopsError> {
        let encrypted = EncryptedValue::parse(self.file, text)?;
        let aad = format!("{}:", path.join(":"));
        let plain = encrypted.open(self.file, &self.cipher, aad.as_bytes())?;
        self.hasher.update(plain.as_slice());
        Ok((encrypted.kind == "str").then_some(plain))
    }

    fn plain_leaf(&mut self, scalar: &Value) -> Option<Zeroizing<Vec<u8>>> {
        if !self.mac_only_encrypted {
            self.hasher.update(scalar_bytes(scalar));
        }
        scalar
            .as_str()
            .map(|text| Zeroizing::new(text.as_bytes().to_vec()))
    }
}

pub(crate) fn verify_mac(
    file: &str,
    metadata: &Mapping,
    data_key: &[u8],
    digest: &str,
) -> Result<(), SopsError> {
    let text_field = |name: &str| {
        metadata
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| SopsError::parse(file, format!("no sops {name}")))
    };
    let encrypted = EncryptedValue::parse(file, text_field("mac")?)?;
    let expected = encrypted.open(
        file,
        &cipher(file, data_key)?,
        text_field("lastmodified")?.as_bytes(),
    )?;

    if expected.eq_ignore_ascii_case(digest.as_bytes()) {
        Ok(())
    } else {
        Err(SopsError::MacMismatch { file: file.into() })
    }
}

fn cipher(file: &str, data_key: &[u8]) -> Result<SopsCipher, SopsError> {
    SopsCipher::new_from_slice(data_key)
        .map_err(|_| SopsError::parse(file, "data key is not 32 bytes"))
}

fn key_text(key: &Value) -> String {
    String::from_utf8_lossy(&scalar_bytes(key)).into_owned()
}

fn scalar_bytes(value: &Value) -> Vec<u8> {
    match value {
        Value::Bool(true) => b"True".to_vec(),
        Value::Bool(false) => b"False".to_vec(),
        Value::Number(number) => number_text(number).into_bytes(),
        Value::String(text) => text.as_bytes().to_vec(),
        _ => Vec::new(),
    }
}

fn number_text(number: &yaml_serde::Number) -> String {
    if let Some(int) = number.as_i64() {
        int.to_string()
    } else if let Some(int) = number.as_u64() {
        int.to_string()
    } else {
        number
            .as_f64()
            .map(|float| float.to_string())
            .unwrap_or_default()
    }
}
