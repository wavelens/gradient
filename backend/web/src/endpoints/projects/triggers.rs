/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! CRUD endpoints for `project_trigger` plus a manual-fire endpoint.

use crate::access::{Caller, ProjectAccess, load_project};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::Permission;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::Utc;
use gradient_core::ci::{apply_trigger, ApplyInput, ApplyOutcome};
use gradient_core::sources::resolve_head;
use gradient_core::types::triggers::{ConcurrencyPolicy, TriggerConfig, TriggerType};
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub fn router() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/{id}", get(read).patch(update).delete(delete_one))
        .route("/{id}/test", post(fire_now))
}

#[derive(Serialize, Debug)]
pub struct TriggerOut {
    pub id: ProjectTriggerId,
    pub project: ProjectId,
    #[serde(rename = "type")]
    pub trigger_type: TriggerType,
    pub concurrency: ConcurrencyPolicy,
    pub config: serde_json::Value,
    pub active: bool,
    pub last_fired_at: Option<chrono::NaiveDateTime>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

impl From<MProjectTrigger> for TriggerOut {
    fn from(m: MProjectTrigger) -> Self {
        Self {
            id: m.id,
            project: m.project,
            trigger_type: TriggerType::from_i16(m.trigger_type).unwrap_or(TriggerType::Polling),
            concurrency: ConcurrencyPolicy::from_i16(m.concurrency)
                .unwrap_or(ConcurrencyPolicy::Skip),
            config: m.config,
            active: m.active,
            last_fired_at: m.last_fired_at,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct CreateBody {
    pub config: TriggerConfig,
    pub concurrency: ConcurrencyPolicy,
    #[serde(default = "default_true")]
    pub active: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize, Debug)]
pub struct UpdateBody {
    pub config: Option<TriggerConfig>,
    pub concurrency: Option<ConcurrencyPolicy>,
    pub active: Option<bool>,
}

#[derive(Serialize, Debug)]
pub struct DeletedResponse {
    pub deleted: bool,
}

fn reject_allow(concurrency: ConcurrencyPolicy) -> WebResult<()> {
    if concurrency == ConcurrencyPolicy::Allow {
        return Err(WebError::bad_request(
            "concurrency `allow` is reserved",
        ));
    }
    Ok(())
}

/// `GET /projects/{org}/{project}/triggers` — list all triggers for the project.
pub async fn list(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<Vec<TriggerOut>>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        organization,
        project,
        ProjectAccess::Member,
    )
    .await?;

    let rows = EProjectTrigger::find()
        .filter(CProjectTrigger::Project.eq(proj.id))
        .all(&state.web_db)
        .await?;

    Ok(ok_json(rows.into_iter().map(TriggerOut::from).collect()))
}

/// `POST /projects/{org}/{project}/triggers` — create a new trigger.
pub async fn create(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project)): Path<(String, String)>,
    Json(body): Json<CreateBody>,
) -> WebResult<Json<BaseResponse<TriggerOut>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        organization,
        project,
        ProjectAccess::Require {
            permission: Permission::EditProject,
            reject_managed: false,
        },
    )
    .await?;

    reject_allow(body.concurrency)?;
    body.config
        .validate()
        .map_err(|e| WebError::bad_request(e.to_string()))?;

    let now = Utc::now().naive_utc();
    let trigger_type = body.config.trigger_type();
    let config_json = body.config.to_db_json();

    let row = AProjectTrigger {
        id: Set(ProjectTriggerId::now_v7()),
        project: Set(proj.id),
        trigger_type: Set(trigger_type.as_i16()),
        concurrency: Set(body.concurrency.as_i16()),
        config: Set(config_json),
        active: Set(body.active),
        last_fired_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&state.web_db)
    .await?;

    Ok(ok_json(TriggerOut::from(row)))
}

/// `GET /projects/{org}/{project}/triggers/{id}` — fetch one trigger.
pub async fn read(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project, id)): Path<(String, String, ProjectTriggerId)>,
) -> WebResult<Json<BaseResponse<TriggerOut>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        organization,
        project,
        ProjectAccess::Member,
    )
    .await?;

    let row = EProjectTrigger::find()
        .filter(CProjectTrigger::Id.eq(id))
        .filter(CProjectTrigger::Project.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Trigger")?;

    Ok(ok_json(TriggerOut::from(row)))
}

/// `PATCH /projects/{org}/{project}/triggers/{id}` — update a trigger.
pub async fn update(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project, id)): Path<(String, String, ProjectTriggerId)>,
    Json(body): Json<UpdateBody>,
) -> WebResult<Json<BaseResponse<TriggerOut>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        organization,
        project,
        ProjectAccess::Require {
            permission: Permission::EditProject,
            reject_managed: false,
        },
    )
    .await?;

    let row = EProjectTrigger::find()
        .filter(CProjectTrigger::Id.eq(id))
        .filter(CProjectTrigger::Project.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Trigger")?;

    if let Some(ref c) = body.concurrency {
        reject_allow(*c)?;
    }
    if let Some(ref cfg) = body.config {
        cfg.validate()
            .map_err(|e| WebError::bad_request(e.to_string()))?;
    }

    let mut active: AProjectTrigger = row.into();
    if let Some(cfg) = body.config {
        let tt = cfg.trigger_type();
        active.trigger_type = Set(tt.as_i16());
        active.config = Set(cfg.to_db_json());
    }
    if let Some(c) = body.concurrency {
        active.concurrency = Set(c.as_i16());
    }
    if let Some(a) = body.active {
        active.active = Set(a);
    }
    active.updated_at = Set(Utc::now().naive_utc());

    let updated = active.update(&state.web_db).await?;

    Ok(ok_json(TriggerOut::from(updated)))
}

/// `DELETE /projects/{org}/{project}/triggers/{id}` — hard delete the trigger.
pub async fn delete_one(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project, id)): Path<(String, String, ProjectTriggerId)>,
) -> WebResult<Json<BaseResponse<DeletedResponse>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        organization,
        project,
        ProjectAccess::Require {
            permission: Permission::EditProject,
            reject_managed: false,
        },
    )
    .await?;

    let row = EProjectTrigger::find()
        .filter(CProjectTrigger::Id.eq(id))
        .filter(CProjectTrigger::Project.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Trigger")?;

    let active: AProjectTrigger = row.into();
    active.delete(&state.web_db).await?;

    Ok(ok_json(DeletedResponse { deleted: true }))
}

/// `POST /projects/{org}/{project}/triggers/{id}/test` — manually fire a trigger.
pub async fn fire_now(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project, id)): Path<(String, String, ProjectTriggerId)>,
) -> WebResult<Json<BaseResponse<serde_json::Value>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        organization,
        project,
        ProjectAccess::Require {
            permission: Permission::TriggerEvaluation,
            reject_managed: false,
        },
    )
    .await?;

    let row = EProjectTrigger::find()
        .filter(CProjectTrigger::Id.eq(id))
        .filter(CProjectTrigger::Project.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found("Trigger")?;

    let trigger_type = TriggerType::from_i16(row.trigger_type)
        .ok_or_else(|| WebError::internal("invalid trigger_type in row"))?;
    let concurrency = ConcurrencyPolicy::from_i16(row.concurrency)
        .ok_or_else(|| WebError::internal("invalid concurrency in row"))?;

    let (commit_hash, commit_message, author_name) = resolve_head(Arc::clone(&state), &proj)
        .await
        .map_err(|e| WebError::internal(e.to_string()))?;

    let input = ApplyInput {
        trigger_id: row.id,
        trigger_type,
        concurrency,
        commit_hash,
        commit_message: Some(commit_message),
        author_name: Some(author_name),
        manual: true,
    };

    let outcome = apply_trigger(&state.web_db, &proj, input)
        .await
        .map_err(|e| WebError::internal(e.to_string()))?;

    let body = match outcome {
        ApplyOutcome::Created(eval) => serde_json::json!({
            "outcome": "Created",
            "evaluation_id": eval.id,
        }),
        ApplyOutcome::SkippedSameCommit => serde_json::json!({ "outcome": "SkippedSameCommit" }),
        ApplyOutcome::SkippedConcurrency => {
            serde_json::json!({ "outcome": "SkippedConcurrency" })
        }
        ApplyOutcome::SkippedAllowReserved => {
            serde_json::json!({ "outcome": "SkippedAllowReserved" })
        }
    };

    Ok(ok_json(body))
}
