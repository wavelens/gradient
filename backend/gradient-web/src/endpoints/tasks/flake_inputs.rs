/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::TaskRefusal;
use crate::error::WebResult;
use crate::helpers::{OptionExt, ok_json};
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use crate::scope::{Member, RequiresUnmanaged, TaskScope, can};
use axum::routing::get;
use axum::{Json, Router};
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
    pub task: TaskId,
    pub input_name: String,
    pub url: Option<String>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

impl From<MTaskFlakeInputOverride> for FlakeInputOverrideOut {
    fn from(m: MTaskFlakeInputOverride) -> Self {
        Self {
            id: m.id,
            task: m.task,
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
        return Err(SharedRefusal::EmptyField("input_name").into());
    }
    let ok = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '*' | '?' | '[' | ']'));
    if !ok {
        return Err(TaskRefusal::InvalidInputNameCharacters.into());
    }
    if !gradient_util::glob::is_pattern(name)
        && let Some(first) = name.chars().next()
        && !first.is_ascii_alphabetic()
        && first != '_'
    {
        return Err(TaskRefusal::InvalidInputNameStart.into());
    }
    Ok(())
}

fn validate_url(url: &Option<String>) -> WebResult<()> {
    if let Some(u) = url
        && u.trim().is_empty()
    {
        return Err(SharedRefusal::EmptyField("url").into());
    }
    Ok(())
}

pub async fn list(
    TaskScope { task: proj, db, .. }: TaskScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<FlakeInputOverrideOut>>>> {
    let rows = ETaskFlakeInputOverride::find()
        .filter(CTaskFlakeInputOverride::Task.eq(proj.id))
        .order_by_asc(CTaskFlakeInputOverride::InputName)
        .all(&db.web_db)
        .await?;

    Ok(ok_json(rows.into_iter().map(Into::into).collect()))
}

pub async fn create(
    TaskScope { task: proj, db, .. }: TaskScope<RequiresUnmanaged<can::EditTask>>,
    Json(body): Json<CreateBody>,
) -> WebResult<Json<BaseResponse<FlakeInputOverrideOut>>> {
    validate_input_name(&body.input_name)?;
    validate_url(&body.url)?;

    let existing = ETaskFlakeInputOverride::find()
        .filter(CTaskFlakeInputOverride::Task.eq(proj.id))
        .filter(CTaskFlakeInputOverride::InputName.eq(&body.input_name))
        .one(&db.web_db)
        .await?;
    if existing.is_some() {
        return Err(TaskRefusal::OverrideExists {
            input_name: body.input_name.clone(),
        }
        .into());
    }

    let now = Utc::now().naive_utc();
    let row = MTaskFlakeInputOverride {
        id: FlakeInputOverrideId::now_v7(),
        task: proj.id,
        input_name: body.input_name,
        url: body.url,
        created_at: now,
        updated_at: now,
    }
    .into_active_model()
    .insert(&db.web_db)
    .await?;

    Ok(ok_json(row.into()))
}

pub async fn read(
    TaskScope {
        task: proj,
        db,
        params,
        ..
    }: TaskScope<Member>,
) -> WebResult<Json<BaseResponse<FlakeInputOverrideOut>>> {
    let id: FlakeInputOverrideId = params.param("id")?;

    let row = ETaskFlakeInputOverride::find()
        .filter(CTaskFlakeInputOverride::Id.eq(id))
        .filter(CTaskFlakeInputOverride::Task.eq(proj.id))
        .one(&db.web_db)
        .await?
        .or_not_found(Resource::FlakeInputOverride)?;

    Ok(ok_json(row.into()))
}

pub async fn update(
    TaskScope {
        task: proj,
        db,
        params,
        ..
    }: TaskScope<RequiresUnmanaged<can::EditTask>>,
    Json(body): Json<UpdateBody>,
) -> WebResult<Json<BaseResponse<FlakeInputOverrideOut>>> {
    let id: FlakeInputOverrideId = params.param("id")?;

    if let Some(n) = &body.input_name {
        validate_input_name(n)?;
    }
    if let Some(Some(u)) = &body.url {
        validate_url(&Some(u.clone()))?;
    }

    let row = ETaskFlakeInputOverride::find()
        .filter(CTaskFlakeInputOverride::Id.eq(id))
        .filter(CTaskFlakeInputOverride::Task.eq(proj.id))
        .one(&db.web_db)
        .await?
        .or_not_found(Resource::FlakeInputOverride)?;

    if let Some(new_name) = &body.input_name
        && new_name != &row.input_name
    {
        let dup = ETaskFlakeInputOverride::find()
            .filter(CTaskFlakeInputOverride::Task.eq(proj.id))
            .filter(CTaskFlakeInputOverride::InputName.eq(new_name))
            .one(&db.web_db)
            .await?;
        if dup.is_some() {
            return Err(TaskRefusal::OverrideExists {
                input_name: new_name.to_string(),
            }
            .into());
        }
    }

    let mut active: ATaskFlakeInputOverride = row.into();
    if let Some(n) = body.input_name {
        active.input_name = Set(n);
    }
    if let Some(u) = body.url {
        active.url = Set(u);
    }
    active.updated_at = Set(Utc::now().naive_utc());

    let updated = active.update(&db.web_db).await?;
    Ok(ok_json(updated.into()))
}

pub async fn delete_one(
    TaskScope {
        task: proj,
        db,
        params,
        ..
    }: TaskScope<RequiresUnmanaged<can::EditTask>>,
) -> WebResult<Json<BaseResponse<DeletedResponse>>> {
    let id: FlakeInputOverrideId = params.param("id")?;

    let row = ETaskFlakeInputOverride::find()
        .filter(CTaskFlakeInputOverride::Id.eq(id))
        .filter(CTaskFlakeInputOverride::Task.eq(proj.id))
        .one(&db.web_db)
        .await?
        .or_not_found(Resource::FlakeInputOverride)?;

    let active: ATaskFlakeInputOverride = row.into();
    active.delete(&db.web_db).await?;

    Ok(ok_json(DeletedResponse { deleted: true }))
}
