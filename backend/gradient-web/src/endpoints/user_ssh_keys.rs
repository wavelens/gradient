/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::audit::Audit;
use crate::authorization::MaybeApiKey;
use crate::endpoints::user::forbid_via_api_key;
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
}

impl Refusal for SshKeyRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::SshDisabled => RefusalStatus::NotFound,
            Self::NotAnOpenSshKey => RefusalStatus::BadRequest,
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

pub fn fingerprint(public_key: &str) -> Result<String, ssh_key::Error> {
    let key = ssh_key::PublicKey::from_openssh(public_key.trim())?;
    Ok(key.fingerprint(ssh_key::HashAlg::Sha256).to_string())
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
    Extension(api_key): Extension<MaybeApiKey>,
    Json(body): Json<CreateSshKeyRequest>,
) -> WebResult<Json<BaseResponse<SshKeyInfo>>> {
    forbid_via_api_key(&api_key)?;
    require_ssh(&state.config)?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(SharedRefusal::EmptyField("name").into());
    }

    let fingerprint = fingerprint(&body.public_key).map_err(|_| SshKeyRefusal::NotAnOpenSshKey)?;

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
    Extension(api_key): Extension<MaybeApiKey>,
    Path(ssh_key_id): Path<UserSshKeyId>,
) -> WebResult<Json<BaseResponse<String>>> {
    forbid_via_api_key(&api_key)?;
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

    #[test]
    fn fingerprint_matches_ssh_keygen() {
        let key =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHfwJ61+Nu5yJhfB3PfAyywWMtpJcwybHAVPzGWnPQ6v test";
        assert_eq!(
            fingerprint(key).expect("key"),
            "SHA256:oriknbp5Jg0jqvSVYJ2XG6wWsyBelapZTV+AMQTOowo"
        );
    }
}
