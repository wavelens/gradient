/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::TaskRefusal;
use crate::error::WebResult;
use crate::helpers::{OptionExt, ok_json};
use crate::resource::Resource;
use crate::scope::{Member, Requires, TaskScope, can};
use anyhow::Context;
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::Utc;
use gradient_ci::{ApplyInput, ApplyOutcome, apply_trigger};
use gradient_core::ServerState;
use gradient_db::lookup::by_ids;
use gradient_graph::Graph;
use gradient_graph::Transition;
use gradient_scheduler::Scheduler;
use gradient_sources::resolve_head;
use gradient_types::triggers::{TriggerConfig, TriggerType};
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

pub fn router() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/{id}", get(read).patch(update).delete(delete_one))
        .route("/{id}/test", post(fire_now))
}

#[derive(Serialize, Debug)]
pub struct TriggerIntegrationSummary {
    pub id: IntegrationId,
    pub name: String,
    pub display_name: String,
    pub git_host_type: String,
}

impl From<&MIntegration> for TriggerIntegrationSummary {
    fn from(m: &MIntegration) -> Self {
        Self {
            id: m.id,
            name: m.name.clone(),
            display_name: m.display_name.clone(),
            git_host_type: m.git_host_type.as_path_segment().to_string(),
        }
    }
}

#[derive(Serialize, Debug)]
pub struct TriggerOut {
    pub id: TaskTriggerId,
    pub task: TaskId,
    #[serde(rename = "type")]
    pub trigger_type: TriggerType,
    pub config: serde_json::Value,
    pub active: bool,
    pub last_fired_at: Option<chrono::NaiveDateTime>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
    pub integration: Option<TriggerIntegrationSummary>,
}

impl TriggerOut {
    fn build(m: MTaskTrigger, integrations: &HashMap<IntegrationId, MIntegration>) -> Self {
        let integration = trigger_integration_id(m.trigger_type, &m.config)
            .and_then(|id| integrations.get(&id))
            .map(TriggerIntegrationSummary::from);
        Self {
            id: m.id,
            task: m.task,
            trigger_type: m.trigger_type,
            config: m.config,
            active: m.active,
            last_fired_at: m.last_fired_at,
            created_at: m.created_at,
            updated_at: m.updated_at,
            integration,
        }
    }
}

fn trigger_integration_id(
    trigger_type: TriggerType,
    config: &serde_json::Value,
) -> Option<IntegrationId> {
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
    rows: &[MTaskTrigger],
) -> WebResult<HashMap<IntegrationId, MIntegration>> {
    let ids: Vec<IntegrationId> = rows
        .iter()
        .filter_map(|t| trigger_integration_id(t.trigger_type, &t.config))
        .collect();
    Ok(by_ids::<EIntegration, _>(db, &ids).await?)
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

pub async fn list(
    TaskScope { task: proj, db, .. }: TaskScope<Member>,
) -> WebResult<Json<BaseResponse<Vec<TriggerOut>>>> {
    let rows = ETaskTrigger::find()
        .filter(CTaskTrigger::Task.eq(proj.id))
        .all(&db.web_db)
        .await?;

    let integrations = load_integrations_for_triggers(&db.web_db, &rows).await?;

    Ok(ok_json(
        rows.into_iter()
            .map(|r| TriggerOut::build(r, &integrations))
            .collect(),
    ))
}

pub async fn create(
    TaskScope { task: proj, db, .. }: TaskScope<Requires<can::ManageTriggers>>,
    Json(body): Json<CreateBody>,
) -> WebResult<Json<BaseResponse<TriggerOut>>> {
    body.config
        .validate()
        .map_err(|e| TaskRefusal::InvalidTriggerConfig(e.to_string()))?;

    let now = Utc::now().naive_utc();
    let trigger_type = body.config.trigger_type();
    let config_json = body.config.to_db_json();

    let row = MTaskTrigger {
        id: TaskTriggerId::now_v7(),
        task: proj.id,
        trigger_type,
        config: config_json,
        active: body.active,
        created_at: now,
        updated_at: now,
        ..Default::default()
    }
    .into_active_model()
    .insert(&db.web_db)
    .await?;

    let integrations =
        load_integrations_for_triggers(&db.web_db, std::slice::from_ref(&row)).await?;
    Ok(ok_json(TriggerOut::build(row, &integrations)))
}

pub async fn read(
    TaskScope {
        task: proj,
        db,
        params,
        ..
    }: TaskScope<Member>,
) -> WebResult<Json<BaseResponse<TriggerOut>>> {
    let id: TaskTriggerId = params.param("id")?;

    let row = ETaskTrigger::find()
        .filter(CTaskTrigger::Id.eq(id))
        .filter(CTaskTrigger::Task.eq(proj.id))
        .one(&db.web_db)
        .await?
        .or_not_found(Resource::Trigger)?;

    let integrations =
        load_integrations_for_triggers(&db.web_db, std::slice::from_ref(&row)).await?;
    Ok(ok_json(TriggerOut::build(row, &integrations)))
}

pub async fn update(
    TaskScope {
        task: proj,
        db,
        params,
        ..
    }: TaskScope<Requires<can::ManageTriggers>>,
    Json(body): Json<UpdateBody>,
) -> WebResult<Json<BaseResponse<TriggerOut>>> {
    let id: TaskTriggerId = params.param("id")?;

    let row = ETaskTrigger::find()
        .filter(CTaskTrigger::Id.eq(id))
        .filter(CTaskTrigger::Task.eq(proj.id))
        .one(&db.web_db)
        .await?
        .or_not_found(Resource::Trigger)?;

    if let Some(ref cfg) = body.config {
        cfg.validate()
            .map_err(|e| TaskRefusal::InvalidTriggerConfig(e.to_string()))?;
    }

    let mut active: ATaskTrigger = row.into();
    if let Some(cfg) = body.config {
        let tt = cfg.trigger_type();
        active.trigger_type = Set(tt);
        active.config = Set(cfg.to_db_json());
    }
    if let Some(a) = body.active {
        active.active = Set(a);
    }
    active.updated_at = Set(Utc::now().naive_utc());

    let updated = active.update(&db.web_db).await?;

    let integrations =
        load_integrations_for_triggers(&db.web_db, std::slice::from_ref(&updated)).await?;
    Ok(ok_json(TriggerOut::build(updated, &integrations)))
}

pub async fn delete_one(
    TaskScope {
        task: proj,
        db,
        params,
        ..
    }: TaskScope<Requires<can::ManageTriggers>>,
) -> WebResult<Json<BaseResponse<DeletedResponse>>> {
    let id: TaskTriggerId = params.param("id")?;

    let row = ETaskTrigger::find()
        .filter(CTaskTrigger::Id.eq(id))
        .filter(CTaskTrigger::Task.eq(proj.id))
        .one(&db.web_db)
        .await?
        .or_not_found(Resource::Trigger)?;

    let active: ATaskTrigger = row.into();
    active.delete(&db.web_db).await?;

    Ok(ok_json(DeletedResponse { deleted: true }))
}

pub async fn fire_now(
    TaskScope {
        task: proj,
        db,
        params,
        ..
    }: TaskScope<Requires<can::TriggerEvaluation>>,
    state: State<Arc<ServerState>>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
) -> WebResult<Json<BaseResponse<serde_json::Value>>> {
    let id: TaskTriggerId = params.param("id")?;

    let row = ETaskTrigger::find()
        .filter(CTaskTrigger::Id.eq(id))
        .filter(CTaskTrigger::Task.eq(proj.id))
        .one(&state.web_db)
        .await?
        .or_not_found(Resource::Trigger)?;

    if !row.active {
        return Err(TaskRefusal::InactiveTrigger.into());
    }

    let trigger_type = row.trigger_type;

    let branch_for_fire: Option<String> = TriggerConfig::parse_row(row.trigger_type, &row.config)
        .ok()
        .and_then(|cfg| match cfg {
            TriggerConfig::Polling { branch, .. } => branch,
            _ => None,
        });

    let (commit_hash, commit_message, author_name) =
        resolve_head(&db, &proj, branch_for_fire.as_deref())
            .await
            .context("resolve the branch head")?;

    if let Err(e) = gradient_ci::trigger::maybe_trigger_input_update(
        &state.web_db,
        &proj,
        commit_hash.clone(),
        Some(row.id),
    )
    .await
    {
        tracing::warn!(error = %e, trigger_id = %row.id, "manual input_update trigger failed");
    }

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
        instance_max_storage_gb: state.config.cache.max_storage_gb,
    };

    let outcome = apply_trigger(&state.web_db, &proj, input)
        .await
        .context("apply the trigger")?;

    {
        let now = gradient_types::now();
        let mut active: gradient_entity::task_trigger::ActiveModel = row.clone().into();
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
            hard_abort,
        } => {
            if let Some(aborted_id) = aborted_evaluation {
                let shared_builds = if hard_abort {
                    abort_eval_shared_builds(&state.graph, aborted_id).await
                } else {
                    Vec::new()
                };

                scheduler
                    .cancel_evaluation_jobs(aborted_id, &shared_builds)
                    .await;
            }

            state.record_evaluation_created(&eval).await;
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

async fn abort_eval_shared_builds(
    graph: &Arc<Graph>,
    evaluation: EvaluationId,
) -> Vec<DerivationBuildId> {
    match graph
        .transition(Transition::AbortEvaluationSharedBuilds { evaluation })
        .await
    {
        Ok(report) => report.aborted_shared_builds,
        Err(e) => {
            tracing::warn!(error = %e, evaluation_id = %evaluation, "aborting the evaluation's shared builds did not reach the graph writer");
            Vec::new()
        }
    }
}
