/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, ProjectAccess, load_project};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::Permission;
use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Extension, Json, Router};
use chrono::Utc;
use gradient_core::ServerState;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub fn router() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/{id}", get(read).patch(update).delete(delete_one))
}

#[derive(Serialize, Debug)]
pub struct FlakeInputOverrideOut {
    pub id: FlakeInputOverrideId,
    pub project: ProjectId,
    pub input_name: String,
    pub url: Option<String>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

impl From<MProjectFlakeInputOverride> for FlakeInputOverrideOut {
    fn from(m: MProjectFlakeInputOverride) -> Self {
        Self {
            id: m.id,
            project: m.project,
            input_name: m.input_name,
            url: m.url,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct CreateBody {
    pub input_name: String,
    pub url: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct UpdateBody {
    pub input_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_optional_string")]
    pub url: Option<Option<String>>,
}

fn deserialize_optional_optional_string<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<String>::deserialize(d)?))
}

#[derive(Serialize, Debug)]
pub struct DeletedResponse {
    pub deleted: bool,
}

fn validate_input_name(name: &str) -> WebResult<()> {
    if name.is_empty() {
        return Err(WebError::bad_request("input_name must not be empty"));
    }
    let ok = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '*' | '?' | '[' | ']'));
    if !ok {
        return Err(WebError::bad_request(
            "input_name may contain letters, digits, _ - and glob chars * ? [ ]",
        ));
    }
    // A literal (non-glob) name must still be a valid flake input identifier.
    if !gradient_util::glob::is_pattern(name) {
        let first = name.chars().next().unwrap();
        if !first.is_ascii_alphabetic() && first != '_' {
            return Err(WebError::bad_request(
                "input_name must match ^[A-Za-z_][A-Za-z0-9_-]*$",
            ));
        }
    }
    Ok(())
}

fn validate_url(url: &Option<String>) -> WebResult<()> {
    if let Some(u) = url
        && u.trim().is_empty()
    {
        return Err(WebError::bad_request("url must not be empty"));
    }
    Ok(())
}

pub async fn list(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<Vec<FlakeInputOverrideOut>>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Member,
    )
    .await?;

    let rows = EProjectFlakeInputOverride::find()
        .filter(CProjectFlakeInputOverride::Project.eq(proj.id))
        .order_by_asc(CProjectFlakeInputOverride::InputName)
        .all(&state.web_db)
        .await?;

    Ok(ok_json(rows.into_iter().map(Into::into).collect()))
}

pub async fn create(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project)): Path<(String, String)>,
    Json(body): Json<CreateBody>,
) -> WebResult<Json<BaseResponse<FlakeInputOverrideOut>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Require {
            permission: Permission::EditProject,
            reject_managed: true,
        },
    )
    .await?;

    validate_input_name(&body.input_name)?;
    validate_url(&body.url)?;

    let existing = EProjectFlakeInputOverride::find()
        .filter(CProjectFlakeInputOverride::Project.eq(proj.id))
        .filter(CProjectFlakeInputOverride::InputName.eq(&body.input_name))
        .one(&state.web_db)
        .await?;
    if existing.is_some() {
        return Err(WebError::bad_request(format!(
            "override for input '{}' already exists",
            body.input_name,
        )));
    }

    let now = Utc::now().naive_utc();
    let row = MProjectFlakeInputOverride {
        id: FlakeInputOverrideId::now_v7(),
        project: proj.id,
        input_name: body.input_name,
        url: body.url,
        created_at: now,
        updated_at: now,
    }
    .into_active_model()
    .insert(&state.web_db)
    .await?;

    Ok(ok_json(row.into()))
}

pub async fn read(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project, id)): Path<(String, String, FlakeInputOverrideId)>,
) -> WebResult<Json<BaseResponse<FlakeInputOverrideOut>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Member,
    )
    .await?;

    let row = EProjectFlakeInputOverride::find()
        .filter(CProjectFlakeInputOverride::Id.eq(id))
        .filter(CProjectFlakeInputOverride::Project.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("FlakeInputOverride")?;

    Ok(ok_json(row.into()))
}

pub async fn update(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project, id)): Path<(String, String, FlakeInputOverrideId)>,
    Json(body): Json<UpdateBody>,
) -> WebResult<Json<BaseResponse<FlakeInputOverrideOut>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Require {
            permission: Permission::EditProject,
            reject_managed: true,
        },
    )
    .await?;

    if let Some(n) = &body.input_name {
        validate_input_name(n)?;
    }
    if let Some(Some(u)) = &body.url {
        validate_url(&Some(u.clone()))?;
    }

    let row = EProjectFlakeInputOverride::find()
        .filter(CProjectFlakeInputOverride::Id.eq(id))
        .filter(CProjectFlakeInputOverride::Project.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("FlakeInputOverride")?;

    if let Some(new_name) = &body.input_name
        && new_name != &row.input_name
    {
        let dup = EProjectFlakeInputOverride::find()
            .filter(CProjectFlakeInputOverride::Project.eq(proj.id))
            .filter(CProjectFlakeInputOverride::InputName.eq(new_name))
            .one(&state.web_db)
            .await?;
        if dup.is_some() {
            return Err(WebError::bad_request(format!(
                "override for input '{new_name}' already exists",
            )));
        }
    }

    let mut active: AProjectFlakeInputOverride = row.into();
    if let Some(n) = body.input_name {
        active.input_name = Set(n);
    }
    if let Some(u) = body.url {
        active.url = Set(u);
    }
    active.updated_at = Set(Utc::now().naive_utc());

    let updated = active.update(&state.web_db).await?;
    Ok(ok_json(updated.into()))
}

pub async fn delete_one(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project, id)): Path<(String, String, FlakeInputOverrideId)>,
) -> WebResult<Json<BaseResponse<DeletedResponse>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Require {
            permission: Permission::EditProject,
            reject_managed: true,
        },
    )
    .await?;

    let row = EProjectFlakeInputOverride::find()
        .filter(CProjectFlakeInputOverride::Id.eq(id))
        .filter(CProjectFlakeInputOverride::Project.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("FlakeInputOverride")?;

    let active: AProjectFlakeInputOverride = row.into();
    active.delete(&state.web_db).await?;

    Ok(ok_json(DeletedResponse { deleted: true }))
}
