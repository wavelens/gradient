/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::audit::{RequestInfo, record as audit_record};
use crate::authorization::MaybeApiKey;
use crate::endpoints::user::forbid_via_api_key;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, ModelTrait, QueryFilter,
    QueryOrder,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize)]
pub struct SshKeyInfo {
    pub id: UserSshKeyId,
    pub name: String,
    pub fingerprint: String,
    pub created_at: NaiveDateTime,
    pub last_used_at: Option<NaiveDateTime>,
}

#[derive(Deserialize)]
pub struct CreateSshKeyRequest {
    pub name: String,
    pub public_key: String,
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

pub fn fingerprint(public_key: &str) -> Result<String, ssh_key::Error> {
    let key = ssh_key::PublicKey::from_openssh(public_key.trim())?;
    Ok(key.fingerprint(ssh_key::HashAlg::Sha256).to_string())
}

fn require_ssh(state: &ServerState) -> WebResult<()> {
    if state.config.ssh.enable {
        Ok(())
    } else {
        Err(WebError::not_found("SSH"))
    }
}

pub async fn get_ssh_keys(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Vec<SshKeyInfo>>>> {
    require_ssh(&state)?;
    let keys = EUserSshKey::find()
        .filter(CUserSshKey::User.eq(user.id))
        .order_by_desc(CUserSshKey::CreatedAt)
        .all(&state.web_db)
        .await?;

    Ok(ok_json(keys.into_iter().map(SshKeyInfo::from).collect()))
}

pub async fn post_ssh_key(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Json(body): Json<CreateSshKeyRequest>,
) -> WebResult<Json<BaseResponse<SshKeyInfo>>> {
    forbid_via_api_key(&api_key)?;
    require_ssh(&state)?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(WebError::bad_request("Name must not be empty."));
    }

    let fingerprint = fingerprint(&body.public_key)
        .map_err(|_| WebError::bad_request("Not an OpenSSH public key."))?;

    if EUserSshKey::find()
        .filter(CUserSshKey::Fingerprint.eq(fingerprint.clone()))
        .one(&state.web_db)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists("SSH key"));
    }

    let inserted = MUserSshKey {
        id: UserSshKeyId::now_v7(),
        user: user.id,
        name: name.to_string(),
        public_key: body.public_key.trim().to_string(),
        fingerprint,
        created_at: now(),
        ..Default::default()
    }
    .into_active_model()
    .insert(&state.web_db)
    .await?;

    audit_record(
        &state,
        Some(user.id),
        Action::SshKeyCreate,
        EventOwner::default(),
        &info,
        Some(serde_json::json!({
            "ssh_key_id": inserted.id.to_string(),
            "fingerprint": inserted.fingerprint,
        })),
    )
    .await;

    Ok(ok_json(inserted.into()))
}

pub async fn delete_ssh_key(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(ssh_key_id): Path<UserSshKeyId>,
) -> WebResult<Json<BaseResponse<String>>> {
    forbid_via_api_key(&api_key)?;
    require_ssh(&state)?;
    let key = EUserSshKey::find_by_id(ssh_key_id)
        .one(&state.web_db)
        .await?
        .filter(|k| k.user == user.id)
        .or_not_found("SSH key")?;

    let key_id = key.id;
    key.delete(&state.web_db).await?;
    audit_record(
        &state,
        Some(user.id),
        Action::SshKeyDelete,
        EventOwner::default(),
        &info,
        Some(serde_json::json!({ "ssh_key_id": key_id.to_string() })),
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
