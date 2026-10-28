/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::str::FromStr as _;

use bech32::{ToBase32 as _, Variant};
use curve25519_dalek::edwards::CompressedEdwardsY;
use gradient_secret::SecretString;
use sha2::{Digest as _, Sha512};
use ssh_key::{Algorithm, PrivateKey, PublicKey};
use zeroize::Zeroizing;

use crate::SopsError;

pub struct SopsKey {
    x25519: Option<age::x25519::Identity>,
    ssh: age::ssh::Identity,
}

impl SopsKey {
    pub fn from_ssh_private_key(openssh: &str) -> Result<SopsKey, SopsError> {
        let openssh = SecretString::new(format!("{}\n", openssh.trim()));
        let private = PrivateKey::from_openssh(openssh.expose()).map_err(key_error)?;
        if private.is_encrypted() {
            return Err(SopsError::Key {
                reason: "passphrase-protected keys are not supported".into(),
            });
        }

        let x25519 = match private.algorithm() {
            Algorithm::Ed25519 => Some(x25519_identity(&private)?),
            Algorithm::Rsa { .. } => None,
            other => return Err(unsupported(&other)),
        };

        Ok(SopsKey {
            x25519,
            ssh: ssh_identity(openssh.expose(), &private)?,
        })
    }

    pub fn age_recipient(&self) -> Option<String> {
        self.x25519
            .as_ref()
            .map(|identity| identity.to_public().to_string())
    }

    pub(crate) fn identities(&self) -> impl Iterator<Item = &dyn age::Identity> {
        self.x25519
            .iter()
            .map(|identity| identity as &dyn age::Identity)
            .chain(std::iter::once(&self.ssh as &dyn age::Identity))
    }
}

fn x25519_identity(private: &PrivateKey) -> Result<age::x25519::Identity, SopsError> {
    let keypair = private.key_data().ed25519().ok_or_else(|| SopsError::Key {
        reason: "missing Ed25519 key data".into(),
    })?;
    let seed = Zeroizing::new(keypair.private.to_bytes());
    let scalar = Zeroizing::new(Sha512::digest(seed.as_slice())[..32].to_vec());
    let encoded = bech32::encode("age-secret-key-", scalar.to_base32(), Variant::Bech32)
        .map_err(key_error)?;
    let encoded = Zeroizing::new(encoded.to_uppercase());
    age::x25519::Identity::from_str(&encoded).map_err(|reason| SopsError::Key {
        reason: reason.to_owned(),
    })
}

fn ssh_identity(openssh: &str, private: &PrivateKey) -> Result<age::ssh::Identity, SopsError> {
    let identity = age::ssh::Identity::from_buffer(openssh.as_bytes(), None).map_err(key_error)?;
    match identity {
        age::ssh::Identity::Unencrypted(_) => Ok(identity),
        age::ssh::Identity::Encrypted(_) => Err(SopsError::Key {
            reason: "passphrase-protected keys are not supported".into(),
        }),
        age::ssh::Identity::Unsupported(_) => Err(unsupported(&private.algorithm())),
    }
}

pub struct SopsRecipient {
    pub age: Option<String>,
    pub ssh: String,
}

pub fn sops_recipient(ssh_public_key: &str) -> Result<SopsRecipient, SopsError> {
    let mut public = PublicKey::from_openssh(ssh_public_key.trim()).map_err(key_error)?;
    public.set_comment("");
    let age = match public.algorithm() {
        Algorithm::Ed25519 => Some(age_recipient_of(&public)?),
        Algorithm::Rsa { .. } => None,
        other => return Err(unsupported(&other)),
    };

    Ok(SopsRecipient {
        age,
        ssh: public.to_openssh().map_err(key_error)?,
    })
}

fn age_recipient_of(public: &PublicKey) -> Result<String, SopsError> {
    let point = public
        .key_data()
        .ed25519()
        .and_then(|key| CompressedEdwardsY(key.0).decompress())
        .ok_or_else(|| SopsError::Key {
            reason: "invalid Ed25519 public key".into(),
        })?;
    bech32::encode(
        "age",
        point.to_montgomery().to_bytes().to_base32(),
        Variant::Bech32,
    )
    .map_err(key_error)
}

fn unsupported(algorithm: &Algorithm) -> SopsError {
    SopsError::UnsupportedKey {
        key_type: algorithm.as_str().to_owned(),
    }
}

fn key_error(error: impl std::fmt::Display) -> SopsError {
    SopsError::Key {
        reason: error.to_string(),
    }
}
