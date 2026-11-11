/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::audit::Audit;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::refusal::{Refusal, RefusalStatus, SharedRefusal};
use crate::resource::Resource;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_db::lookup::by_id;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
enum SshKeyRefusal {
    #[error("SSH not found")]
    SshDisabled,
    #[error("Not an OpenSSH public key.")]
    NotAnOpenSshKey,
    #[error("DSA keys are not supported. Use an Ed25519 or ECDSA key.")]
    DsaNotSupported,
    #[error("RSA keys need at least {MIN_RSA_BITS} bits.")]
    WeakRsaKey,
}

impl Refusal for SshKeyRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::SshDisabled => RefusalStatus::NotFound,
            Self::NotAnOpenSshKey | Self::DsaNotSupported | Self::WeakRsaKey => {
                RefusalStatus::BadRequest
            }
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SshKeyInfo {
    pub id: UserSshKeyId,
    pub name: String,
    pub fingerprint: String,
    pub created_at: NaiveDateTime,
    pub last_used_at: Option<NaiveDateTime>,
}

impl From<MUserSshKey> for SshKeyInfo {
    fn from(key: MUserSshKey) -> Self {
        Self {
            id: key.id,
            name: key.name,
            fingerprint: key.fingerprint,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
        }
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateSshKeyRequest {
    pub name: String,
    pub public_key: String,
}

const MIN_RSA_BITS: usize = 2048;

fn fingerprint(public_key: &str) -> Result<String, SshKeyRefusal> {
    let public_key = public_key.trim();

    if public_key
        .split_whitespace()
        .next()
        .is_some_and(|algorithm| algorithm == "ssh-dss")
    {
        return Err(SshKeyRefusal::DsaNotSupported);
    }

    let key =
        ssh_key::PublicKey::from_openssh(public_key).map_err(|_| SshKeyRefusal::NotAnOpenSshKey)?;

    if let ssh_key::public::KeyData::Rsa(rsa) = key.key_data()
        && rsa_bits(&rsa.n) < MIN_RSA_BITS
    {
        return Err(SshKeyRefusal::WeakRsaKey);
    }

    Ok(key.fingerprint(ssh_key::HashAlg::Sha256).to_string())
}

fn rsa_bits(modulus: &ssh_key::Mpint) -> usize {
    let bytes = modulus.as_bytes();
    let Some(start) = bytes.iter().position(|byte| *byte != 0) else {
        return 0;
    };
    let significant = &bytes[start..];

    (significant.len() - 1) * 8 + (8 - significant[0].leading_zeros() as usize)
}

fn require_ssh(config: &RuntimeConfig) -> WebResult<()> {
    if config.ssh.enable {
        Ok(())
    } else {
        Err(SshKeyRefusal::SshDisabled.into())
    }
}

#[utoipa::path(
    get,
    path = "/user/ssh-keys",
    operation_id = "getUserSshKeys",
    tag = "user",
    summary = "List SSH keys",
    description = "Returns the SSH public keys of the authenticated user. Each key may log in\nto the Nix daemon over SSH (`ssh-ng://<project>@<host>`). `404` when SSH is disabled.",
    responses((status = OK, description = "List of SSH keys", body = Vec<SshKeyInfo>), WebError)
)]
pub async fn get_ssh_keys(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Vec<SshKeyInfo>>>> {
    require_ssh(&state.config)?;
    let keys = gradient_db::ssh_keys::of_user(&state.web_db, user.id).await?;

    Ok(ok_json(keys.into_iter().map(SshKeyInfo::from).collect()))
}

#[utoipa::path(
    post,
    path = "/user/ssh-keys",
    operation_id = "createUserSshKey",
    tag = "user",
    summary = "Add SSH key",
    description = "Adds an OpenSSH public key. A key can belong to one user only.",
    request_body = CreateSshKeyRequest,
    responses((status = OK, description = "Key added", body = SshKeyInfo), WebError)
)]
pub async fn post_ssh_key(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Json(body): Json<CreateSshKeyRequest>,
) -> WebResult<Json<BaseResponse<SshKeyInfo>>> {
    require_ssh(&state.config)?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(SharedRefusal::EmptyField("name").into());
    }

    let fingerprint = fingerprint(&body.public_key)?;

    if gradient_db::ssh_keys::by_fingerprint(&state.web_db, &fingerprint)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(Resource::SshKey));
    }

    let inserted = gradient_db::rows::insert::<EUserSshKey, _>(
        &state.web_db,
        MUserSshKey {
            id: UserSshKeyId::now_v7(),
            user: user.id,
            name: name.to_string(),
            public_key: body.public_key.trim().to_string(),
            fingerprint,
            created_at: now(),
            ..Default::default()
        },
    )
    .await
    .map_err(|e| WebError::from_db_err(e, Resource::SshKey))?;

    audit.write(Action::SshKeyCreate, EventOwner::default(), serde_json::json!({ "ssh_key_id": inserted.id.to_string(), "fingerprint": inserted.fingerprint, }))
    .await;

    Ok(ok_json(inserted.into()))
}

#[utoipa::path(
    delete,
    path = "/user/ssh-keys/{ssh_key_id}",
    operation_id = "deleteUserSshKey",
    tag = "user",
    summary = "Delete SSH key",
    params(("ssh_key_id" = UserSshKeyId, Path)),
    responses((status = OK, description = "Key deleted", body = String, content_type = "application/json"), WebError)
)]
pub async fn delete_ssh_key(
    state: State<Arc<ServerState>>,
    audit: Audit,
    Extension(user): Extension<MUser>,
    Path(ssh_key_id): Path<UserSshKeyId>,
) -> WebResult<Json<BaseResponse<String>>> {
    require_ssh(&state.config)?;
    let key = by_id::<EUserSshKey, _>(&state.web_db, ssh_key_id)
        .await?
        .filter(|k| k.user == user.id)
        .or_not_found(Resource::SshKey)?;

    let key_id = key.id;
    gradient_db::rows::delete::<EUserSshKey, _>(&state.web_db, key).await?;
    audit
        .write(
            Action::SshKeyDelete,
            EventOwner::default(),
            serde_json::json!({ "ssh_key_id": key_id.to_string() }),
        )
        .await;

    Ok(ok_json("SSH key deleted".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED25519: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHfwJ61+Nu5yJhfB3PfAyywWMtpJcwybHAVPzGWnPQ6v test";
    const ECDSA_P256: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBH7e0qqxxIFm8N39DMwZjKb/7Wz1kgx6zp5fpEQuKUIL/vNgwBT7aY+LQFlAZg1hQLcKZFV8/pwYiR4tvCvgPyM= test-p256";
    const ECDSA_P384: &str = "ecdsa-sha2-nistp384 AAAAE2VjZHNhLXNoYTItbmlzdHAzODQAAAAIbmlzdHAzODQAAABhBFS0f4aHfIJfZ2AdKId98K4x+JvglmnOG7u2jNNdOUBj2nlwUDXdIm043nOxH0/mi13mDL7rbeLQNHGTePzTDYFLTHNfeXLxpicWCYQn3uJys8VMwKKEqQWIxE0J5f02ug== test-p384";
    const ECDSA_P521: &str = "ecdsa-sha2-nistp521 AAAAE2VjZHNhLXNoYTItbmlzdHA1MjEAAAAIbmlzdHA1MjEAAACFBABx7W7JoAEapctvZfVTuxlhfM8JdzpV6S3nLmmXDp3G8Eq+zWWxC6wVXTyTycixpkBWskBEbmZZliaAdnI+lL8GcQDSg9kVI5K1tARCYcvtTCHDJVkJTQ1vHV6+kx0irruA/AhxyW2bKTYcGCGemEvCkPpfmMj5kSEiYfKT63/UGF807w== test-p521";
    const RSA_2048: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQC7VF7hHqPBCJyRg+jWqJXN6nHaNSKk59LCM+j72R9VihFN7vdwr5KcEeGsOsCnrHIz5rGNlOb+ensZ6oB7bIUoRdnFWKllZHPJ8eZ0WkFP7PvKvSbGEmdhdUOp9mrR3dqCmdc+1ghuzGmTXgeVPc3tBIB64EkRA48Fm8Soges4GEITAPvBoMOBQvRws3VqWCMbYDOpxJE1kV8JyywDQFeypXg7qVdN0uHWUQunqOM6yFqn7ob3Vw5J0BUaLX0Hm7q2jihpTzxN7upQ/M6W/FFjSocJPXk3GZgyR2XDSUzPKtGC1bevcclalLGPzctzf77Dzjq2wAENV5gHjB5YsI4t test-rsa2048";
    const RSA_1024: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAAgQDOEMBO6fopKht6WW+NdD8mxX6RZNA+0ngdNkOZ/DcA2iFY2D6ujXMfRtOUT//m62FAPcgfA+p/KqopPBCkBR8DA2v5E1lNE+W4arVknbwfUstxor8Xnhm/qG+gNLqIlWoRkXKFWjku0iM7ydsmELFpbxzz9+w96qIgHdgjPljRKQ== test-rsa1024";
    const DSA: &str = "ssh-dss AAAAB3NzaC1kc3MAAACBAP test-dsa";

    #[test]
    fn supported_keys_match_ssh_keygen_and_unsafe_ones_are_refused() {
        let supported = [
            (
                ED25519,
                "SHA256:oriknbp5Jg0jqvSVYJ2XG6wWsyBelapZTV+AMQTOowo",
            ),
            (
                ECDSA_P256,
                "SHA256:jmbSbrUIr3Se1zoUXHWvSgjmb/yWgjcyKxObKwzD5aA",
            ),
            (
                ECDSA_P384,
                "SHA256:s7Xl8ASRQChJrTOBQnKJOD2xBpgo+CIwaEZeSDrzjB0",
            ),
            (
                ECDSA_P521,
                "SHA256:qp4UnVqQ8ZrHSnassSPRPyuOMllzvBJ5mPbFTnfwfuI",
            ),
            (
                RSA_2048,
                "SHA256:dOlO2j/l/WNPjdDusxHrpgV13TVM/kyTKkY1Qmyj5WI",
            ),
        ];

        for (key, fingerprint_openssh) in supported {
            assert_eq!(
                fingerprint(key).expect("supported key"),
                fingerprint_openssh,
                "{key}"
            );
        }

        let refused = [
            (RSA_1024, SshKeyRefusal::WeakRsaKey),
            (DSA, SshKeyRefusal::DsaNotSupported),
            ("ssh-ed25519 garbage", SshKeyRefusal::NotAnOpenSshKey),
            ("", SshKeyRefusal::NotAnOpenSshKey),
            (
                "ssh-rsa AAAAB3NzaC1yc2E= truncated",
                SshKeyRefusal::NotAnOpenSshKey,
            ),
        ];

        for (key, expected) in refused {
            assert_eq!(
                fingerprint(key).expect_err("refused key").to_string(),
                expected.to_string(),
                "{key}"
            );
        }
    }
}
