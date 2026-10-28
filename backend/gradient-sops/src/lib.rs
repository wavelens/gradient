/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod document;
mod key;
mod mac;

use std::collections::BTreeMap;

use zeroize::Zeroizing;

pub use document::decrypt_file;
pub use key::{SopsKey, SopsRecipient, sops_recipient};

pub struct SopsSecrets(BTreeMap<String, Zeroizing<Vec<u8>>>);

impl SopsSecrets {
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.0.get(name).map(|value| value.as_slice())
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }
}

#[derive(thiserror::Error, Debug)]
pub enum SopsError {
    #[error("sops file {file}: only YAML and JSON are supported")]
    UnsupportedFormat { file: String },
    #[error("sops file {file}: {reason}")]
    Parse { file: String, reason: String },
    #[error("sops file {file}: not encrypted for the project key")]
    NotARecipient { file: String },
    #[error("sops file {file}: MAC mismatch")]
    MacMismatch { file: String },
    #[error(
        "sops file {file}: encrypted comments not supported, remove the comments before encrypting"
    )]
    EncryptedComments { file: String },
    #[error("{key_type} keys cannot decrypt sops files, use an Ed25519 or RSA key")]
    UnsupportedKey { key_type: String },
    #[error("unreadable SSH key: {reason}")]
    Key { reason: String },
}

impl SopsError {
    pub(crate) fn parse(file: &str, reason: impl Into<String>) -> Self {
        Self::Parse {
            file: file.into(),
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed25519() -> SopsKey {
        SopsKey::from_ssh_private_key(include_str!("../tests/fixtures/ed25519"))
            .expect("ed25519 fixture key")
    }

    #[test]
    fn a_yaml_file_decrypts_with_the_project_key() {
        let secrets = decrypt_file(
            "secrets/train.yaml",
            include_bytes!("../tests/fixtures/secrets.yaml"),
            &ed25519(),
        )
        .expect("decrypts");
        assert_eq!(secrets.get("wandb-token"), Some(b"s3cr3t-value".as_slice()));
        assert_eq!(secrets.get("nested"), None);
        assert_eq!(secrets.get("note_unencrypted"), Some(b"hello".as_slice()));
    }

    #[test]
    fn a_stored_project_key_with_a_trailing_blank_line_decrypts() {
        let stored = format!("{}\n", include_str!("../tests/fixtures/ed25519"));
        let key = SopsKey::from_ssh_private_key(&stored).expect("key with a blank line");
        let secrets = decrypt_file(
            "secrets/train.yaml",
            include_bytes!("../tests/fixtures/secrets.yaml"),
            &key,
        )
        .expect("decrypts");
        assert_eq!(secrets.get("wandb-token"), Some(b"s3cr3t-value".as_slice()));
    }

    #[test]
    fn a_json_file_decrypts_with_the_project_key() {
        let secrets = decrypt_file(
            "secrets/train.json",
            include_bytes!("../tests/fixtures/secrets.json"),
            &ed25519(),
        )
        .expect("decrypts");
        assert_eq!(secrets.get("wandb-token"), Some(b"s3cr3t-value".as_slice()));
        assert_eq!(secrets.get("nested"), None);
        assert_eq!(secrets.get("note_unencrypted"), Some(b"hello".as_slice()));
    }

    #[test]
    fn a_file_for_another_key_names_itself() {
        let err = decrypt_file(
            "secrets/train.yaml",
            include_bytes!("../tests/fixtures/other.yaml"),
            &ed25519(),
        )
        .err()
        .expect("refused");
        assert_eq!(
            err.to_string(),
            "sops file secrets/train.yaml: not encrypted for the project key"
        );
    }

    #[test]
    fn a_changed_value_fails_the_mac() {
        let err = decrypt_file(
            "a.yaml",
            include_bytes!("../tests/fixtures/tampered.yaml"),
            &ed25519(),
        )
        .err()
        .expect("refused");
        assert_eq!(err.to_string(), "sops file a.yaml: MAC mismatch");
    }

    #[test]
    fn encrypted_comments_are_refused_with_a_message() {
        let err = decrypt_file(
            "c.yaml",
            include_bytes!("../tests/fixtures/commented.yaml"),
            &ed25519(),
        )
        .err()
        .expect("refused");
        assert!(matches!(err, SopsError::EncryptedComments { .. }), "{err}");
    }

    #[test]
    fn a_toml_file_is_refused() {
        assert!(matches!(
            decrypt_file("a.toml", b"", &ed25519()),
            Err(SopsError::UnsupportedFormat { .. })
        ));
    }

    #[test]
    fn the_age_recipient_matches_ssh_to_age_and_the_private_key() {
        let expected = include_str!("../tests/fixtures/ed25519.age-recipient").trim();
        let recipient =
            sops_recipient(include_str!("../tests/fixtures/ed25519.pub")).expect("recipient");
        assert_eq!(recipient.age.as_deref(), Some(expected));
        assert_eq!(ed25519().age_recipient().as_deref(), Some(expected));
    }

    #[test]
    fn an_rsa_key_decrypts_its_ssh_stanza_and_has_no_age_recipient() {
        let key = SopsKey::from_ssh_private_key(include_str!("../tests/fixtures/rsa"))
            .expect("rsa fixture key");
        let secrets = decrypt_file("r.yaml", include_bytes!("../tests/fixtures/rsa.yaml"), &key)
            .expect("decrypts");
        assert!(secrets.get("wandb-token").is_some());
        let recipient =
            sops_recipient(include_str!("../tests/fixtures/rsa.pub")).expect("recipient");
        assert_eq!(recipient.age, None);
    }
}
