/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! CRUD endpoints for `project_trigger` plus a manual-fire endpoint.

use crate::access::{Caller, ProjectAccess, load_project};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::permissions::Permission;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::Utc;
use gradient_core::ci::{ApplyInput, ApplyOutcome, apply_trigger};
use gradient_types::ForgeType;
use gradient_sources::resolve_head;
use gradient_types::triggers::{TriggerConfig, TriggerType};
use gradient_types::*;
use gradient_core::ServerState;
use gradient_scheduler::Scheduler;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

pub fn router() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/{id}", get(read).patch(update).delete(delete_one))
        .route("/{id}/test", post(fire_now))
}

/// Slim integration handle inlined into reporter trigger responses so the
/// trigger list is renderable without a second (admin-gated) call to the
/// integrations endpoint.
#[derive(Serialize, Debug)]
pub struct TriggerIntegrationSummary {
    pub id: IntegrationId,
    pub name: String,
    pub display_name: String,
    pub forge_type: String,
}

impl TriggerIntegrationSummary {
    fn from_model(m: &MIntegration) -> Self {
        Self {
            id: m.id,
            name: m.name.clone(),
            display_name: m.display_name.clone(),
            forge_type: forge_to_str(m.forge_type).to_string(),
        }
    }
}

fn forge_to_str(f: i16) -> &'static str {
    ForgeType::try_from(f)
        .map(ForgeType::as_path_segment)
        .unwrap_or("unknown")
}

#[derive(Serialize, Debug)]
pub struct TriggerOut {
    pub id: ProjectTriggerId,
    pub project: ProjectId,
    #[serde(rename = "type")]
    pub trigger_type: TriggerType,
    pub config: serde_json::Value,
    pub active: bool,
    pub last_fired_at: Option<chrono::NaiveDateTime>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
    /// Populated for `reporter_push` / `reporter_pull_request` triggers when
    /// the referenced integration row still exists. `null` for polling/time
    /// triggers and for orphaned references (integration deleted out from
    /// under the trigger).
    pub integration: Option<TriggerIntegrationSummary>,
}

impl TriggerOut {
    fn build(m: MProjectTrigger, integrations: &HashMap<IntegrationId, MIntegration>) -> Self {
        let integration = trigger_integration_id(m.trigger_type, &m.config)
            .and_then(|id| integrations.get(&id))
            .map(TriggerIntegrationSummary::from_model);
        Self {
            id: m.id,
            project: m.project,
            trigger_type: TriggerType::try_from(m.trigger_type).unwrap_or(TriggerType::Polling),
            config: m.config,
            active: m.active,
            last_fired_at: m.last_fired_at,
            created_at: m.created_at,
            updated_at: m.updated_at,
            integration,
        }
    }
}

fn trigger_integration_id(trigger_type: i16, config: &serde_json::Value) -> Option<IntegrationId> {
    TriggerConfig::parse_row(trigger_type, config)
        .ok()
        .and_then(|cfg| match cfg {
            TriggerConfig::ReporterPush { integration_id, .. }
            | TriggerConfig::ReporterPullRequest { integration_id, .. } => Some(integration_id),
            _ => None,
        })
}

async fn load_integrations_for_triggers<C: ConnectionTrait>(
    db: &C,
    rows: &[MProjectTrigger],
) -> WebResult<HashMap<IntegrationId, MIntegration>> {
    let ids: Vec<IntegrationId> = rows
        .iter()
        .filter_map(|t| trigger_integration_id(t.trigger_type, &t.config))
        .collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let integrations = EIntegration::find()
        .filter(CIntegration::Id.is_in(ids))
        .all(db)
        .await?;
    Ok(integrations.into_iter().map(|i| (i.id, i)).collect())
}

#[derive(Deserialize, Debug)]
pub struct CreateBody {
    pub config: TriggerConfig,
    #[serde(default = "default_true")]
    pub active: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize, Debug)]
pub struct UpdateBody {
    pub config: Option<TriggerConfig>,
    pub active: Option<bool>,
}

#[derive(Serialize, Debug)]
pub struct DeletedResponse {
    pub deleted: bool,
}

/// `GET /projects/{org}/{project}/triggers` - list all triggers for the project.
pub async fn list(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<Vec<TriggerOut>>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Member,
    )
    .await?;

    let rows = EProjectTrigger::find()
        .filter(CProjectTrigger::Project.eq(proj.id))
        .all(&state.web_db)
        .await?;

    let integrations = load_integrations_for_triggers(&state.web_db, &rows).await?;

    Ok(ok_json(
        rows.into_iter()
            .map(|r| TriggerOut::build(r, &integrations))
            .collect(),
    ))
}

/// `POST /projects/{org}/{project}/triggers` - create a new trigger.
pub async fn create(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project)): Path<(String, String)>,
    Json(body): Json<CreateBody>,
) -> WebResult<Json<BaseResponse<TriggerOut>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Require {
            permission: Permission::ManageTriggers,
            reject_managed: false,
        },
    )
    .await?;

    body.config
        .validate()
        .map_err(|e| WebError::bad_request(e.to_string()))?;

    let now = Utc::now().naive_utc();
    let trigger_type = body.config.trigger_type();
    let config_json = body.config.to_db_json();

    let row = AProjectTrigger {
        id: Set(ProjectTriggerId::now_v7()),
        project: Set(proj.id),
        trigger_type: Set(i16::from(trigger_type)),
        config: Set(config_json),
        active: Set(body.active),
        last_fired_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&state.web_db)
    .await?;

    let integrations =
        load_integrations_for_triggers(&state.web_db, std::slice::from_ref(&row)).await?;
    Ok(ok_json(TriggerOut::build(row, &integrations)))
}

/// `GET /projects/{org}/{project}/triggers/{id}` - fetch one trigger.
pub async fn read(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project, id)): Path<(String, String, ProjectTriggerId)>,
) -> WebResult<Json<BaseResponse<TriggerOut>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
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

    let integrations =
        load_integrations_for_triggers(&state.web_db, std::slice::from_ref(&row)).await?;
    Ok(ok_json(TriggerOut::build(row, &integrations)))
}

/// `PATCH /projects/{org}/{project}/triggers/{id}` - update a trigger.
pub async fn update(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project, id)): Path<(String, String, ProjectTriggerId)>,
    Json(body): Json<UpdateBody>,
) -> WebResult<Json<BaseResponse<TriggerOut>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Require {
            permission: Permission::ManageTriggers,
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

    if let Some(ref cfg) = body.config {
        cfg.validate()
            .map_err(|e| WebError::bad_request(e.to_string()))?;
    }

    let mut active: AProjectTrigger = row.into();
    if let Some(cfg) = body.config {
        let tt = cfg.trigger_type();
        active.trigger_type = Set(i16::from(tt));
        active.config = Set(cfg.to_db_json());
    }
    if let Some(a) = body.active {
        active.active = Set(a);
    }
    active.updated_at = Set(Utc::now().naive_utc());

    let updated = active.update(&state.web_db).await?;

    let integrations =
        load_integrations_for_triggers(&state.web_db, std::slice::from_ref(&updated)).await?;
    Ok(ok_json(TriggerOut::build(updated, &integrations)))
}

/// `DELETE /projects/{org}/{project}/triggers/{id}` - hard delete the trigger.
pub async fn delete_one(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((organization, project, id)): Path<(String, String, ProjectTriggerId)>,
) -> WebResult<Json<BaseResponse<DeletedResponse>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        project,
        ProjectAccess::Require {
            permission: Permission::ManageTriggers,
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

/// `POST /projects/{org}/{project}/triggers/{id}/test` - manually fire a trigger.
pub async fn fire_now(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path((organization, project, id)): Path<(String, String, ProjectTriggerId)>,
) -> WebResult<Json<BaseResponse<serde_json::Value>>> {
    let (_org, proj) = load_project(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
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

    if !row.active {
        return Err(WebError::bad_request("trigger is inactive"));
    }

    let trigger_type = TriggerType::try_from(row.trigger_type)
        .map_err(|_| WebError::internal("invalid trigger_type in row"))?;

    let branch_for_fire: Option<String> = TriggerConfig::parse_row(row.trigger_type, &row.config)
        .ok()
        .and_then(|cfg| match cfg {
            TriggerConfig::Polling { branch, .. } => branch,
            _ => None,
        });

    let (commit_hash, commit_message, author_name) =
        resolve_head(&state.db(), &proj, branch_for_fire.as_deref())
            .await
            .map_err(|e| WebError::internal(e.to_string()))?;

    let input = ApplyInput {
        trigger_id: row.id,
        trigger_type,
        commit_hash,
        commit_message: Some(commit_message),
        author_name: Some(author_name),
        manual: true,
        gate_approval: None,
        repository_override: None,
        wildcard_override: None,
        source_comment: None,
        instance_max_storage_gb: state.config.storage.max_storage_gb,
    };

    let outcome = apply_trigger(&state.web_db, &proj, input)
        .await
        .map_err(|e| WebError::internal(e.to_string()))?;

    // Stamp last_fired_at so the UI reflects the manual fire alongside the
    // outcome - mirrors the touch in the webhook fan-out path.
    {
        let now = gradient_types::now();
        let mut active: gradient_entity::project_trigger::ActiveModel = row.clone().into();
        active.last_fired_at = Set(Some(now));
        active.updated_at = Set(now);
        if let Err(e) = active.update(&state.web_db).await {
            tracing::warn!(error = %e, trigger_id = %row.id, "failed to stamp trigger last_fired_at");
        }
    }

    let body = match outcome {
        ApplyOutcome::Created {
            evaluation: eval,
            aborted_evaluation,
            aborted_builds,
        } => {
            if let Some(aborted_id) = aborted_evaluation {
                scheduler
                    .cancel_evaluation_jobs(aborted_id, &aborted_builds)
                    .await;
            }
            gradient_core::ci::actions::dispatch_evaluation_created(&state.ci(), &eval).await;
            serde_json::json!({
                "outcome": "Created",
                "evaluation_id": eval.id,
            })
        }
        ApplyOutcome::SkippedSameCommit => serde_json::json!({ "outcome": "SkippedSameCommit" }),
        ApplyOutcome::SkippedConcurrency => {
            serde_json::json!({ "outcome": "SkippedConcurrency" })
        }
    };

    Ok(ok_json(body))
}
