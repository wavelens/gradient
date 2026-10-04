/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{TeamAccess, load_team};
use crate::audit::{RequestInfo, record as audit_record};
use crate::authorization::MaybeApiKey;
use crate::endpoints::projects::workers::{
    PatchWorkerRequest, RegisterWorkerRequest, WorkerConnection, encrypt_for_dialing, live_workers,
    patch_edits_managed_fields, worker_connection,
};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_scheduler::Scheduler;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize)]
pub struct TeamWorkerEntry {
    pub worker_id: String,
    pub display_name: String,
    pub registered_at: NaiveDateTime,
    pub active: bool,
    pub managed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub gradient_ci: bool,
    pub enable_fetch: bool,
    pub enable_eval: bool,
    pub enable_build: bool,
    #[serde(flatten)]
    pub connection: WorkerConnection,
}

#[derive(Serialize)]
pub struct RegisteredTeamWorker {
    pub team: TeamId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

pub async fn reauth_team_workers(scheduler: &Scheduler, workers: &[String]) {
    for worker_id in workers {
        scheduler.request_reauth(worker_id).await;
    }
}

pub async fn withdraw_team_workers(
    scheduler: &Scheduler,
    workers: &[String],
    projects: &HashSet<ProjectId>,
) {
    for worker_id in workers {
        scheduler
            .abort_project_jobs_on_worker(worker_id, projects)
            .await;
        scheduler.request_reauth(worker_id).await;
    }
}

async fn granted_projects(state: &Arc<ServerState>, team: TeamId) -> WebResult<HashSet<ProjectId>> {
    Ok(
        gradient_db::teams::workers::projects_granted_with_workers(&state.web_db, team)
            .await?
            .into_iter()
            .collect(),
    )
}

async fn find_team_worker(
    state: &Arc<ServerState>,
    team: TeamId,
    worker_id: &str,
) -> WebResult<MTeamWorker> {
    ETeamWorker::find()
        .filter(CTeamWorker::Team.eq(team))
        .filter(CTeamWorker::WorkerId.eq(worker_id))
        .one(&state.web_db)
        .await?
        .or_not_found("Team worker")
}

pub async fn get_team_workers(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path(team): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<TeamWorkerEntry>>>> {
    let (team, role) = load_team(&state, &user, api_key.as_ref(), team, TeamAccess::Member).await?;
    let failures = (user.superuser || role == Some(TeamRole::Admin))
        .then_some(&*scheduler.connection_failures);
    let live_workers = live_workers(scheduler.workers_info().await);
    let rows = ETeamWorker::find()
        .filter(CTeamWorker::Team.eq(team.id))
        .all(&state.web_db)
        .await?;

    let mut entries = Vec::with_capacity(rows.len());
    for worker in rows {
        let connection = worker_connection(&live_workers, failures, &worker.worker_id);
        entries.push(TeamWorkerEntry {
            worker_id: worker.worker_id,
            display_name: worker.display_name,
            registered_at: worker.created_at,
            active: worker.active,
            managed: worker.managed,
            url: worker.url,
            gradient_ci: worker.gradient_ci,
            enable_fetch: worker.enable_fetch,
            enable_eval: worker.enable_eval,
            enable_build: worker.enable_build,
            connection,
        });
    }

    Ok(ok_json(entries))
}

pub async fn post_team_worker(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path(team): Path<String>,
    Json(body): Json<RegisterWorkerRequest>,
) -> WebResult<Json<BaseResponse<RegisteredTeamWorker>>> {
    let (team, _) = load_team(
        &state,
        &user,
        api_key.as_ref(),
        team,
        TeamAccess::Admin {
            reject_managed: false,
        },
    )
    .await?;

    let worker_id = Uuid::parse_str(&body.worker_id)
        .map_err(|_| WebError::bad_request("worker_id must be a valid UUID"))?
        .to_string();
    let registered = EWorkerRegistration::find()
        .filter(CWorkerRegistration::WorkerId.eq(&worker_id))
        .one(&state.web_db)
        .await?
        .is_some();
    if registered || gradient_db::teams::workers::is_team_worker(&state.web_db, &worker_id).await? {
        return Err(WebError::conflict("the worker id is already registered"));
    }

    let (token, return_token) = crate::endpoints::worker_tokens::issue(body.token)?;
    let token_encrypted = encrypt_for_dialing(
        &state.config.secrets.crypt_file,
        body.url.as_deref(),
        &token,
    )?;

    MTeamWorker {
        id: TeamWorkerId::now_v7(),
        team: team.id,
        worker_id: worker_id.clone(),
        token_hash: password_auth::generate_hash(&token),
        token_encrypted,
        url: body.url,
        display_name: body.display_name.trim().to_string(),
        gradient_ci: false,
        enable_fetch: body.enable_fetch,
        enable_eval: body.enable_eval,
        enable_build: body.enable_build,
        active: true,
        managed: false,
        created_by: Some(user.id),
        created_at: gradient_types::now(),
    }
    .into_active_model()
    .insert(&state.web_db)
    .await?;

    scheduler.request_reauth(&worker_id).await;
    for project in granted_projects(&state, team.id).await? {
        if let Err(e) = gradient_ci::unpark_no_workers_for_project(&state.web_db, project).await {
            tracing::warn!(error = %e, project_id = %project, "failed to unpark evaluations after a team worker registration");
        }
    }

    audit_record(
        &state,
        Some(user.id),
        Action::TeamWorkerCreate,
        EventOwner::default(),
        &info,
        Some(serde_json::json!({ "team_id": team.id.to_string(), "worker_id": worker_id })),
    )
    .await;

    Ok(ok_json(RegisteredTeamWorker {
        team: team.id,
        token: return_token.then_some(token),
    }))
}

pub async fn patch_team_worker(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path((team, worker_id)): Path<(String, String)>,
    Json(body): Json<PatchWorkerRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (team, _) = load_team(
        &state,
        &user,
        api_key.as_ref(),
        team,
        TeamAccess::Admin {
            reject_managed: false,
        },
    )
    .await?;
    let worker = find_team_worker(&state, team.id, &worker_id).await?;
    if worker.managed && patch_edits_managed_fields(&body) {
        return Err(WebError::conflict("worker is managed by server state"));
    }

    let mut active: ATeamWorker = worker.into();
    if let Some(value) = body.active {
        active.active = Set(value);
    }
    if let Some(name) = body.display_name {
        active.display_name = Set(name.trim().to_string());
    }
    if let Some(value) = body.enable_fetch {
        active.enable_fetch = Set(value);
    }
    if let Some(value) = body.enable_eval {
        active.enable_eval = Set(value);
    }
    if let Some(value) = body.enable_build {
        active.enable_build = Set(value);
    }
    active.update(&state.web_db).await?;

    let workers = [worker_id.clone()];
    if body.active == Some(false) {
        withdraw_team_workers(
            &scheduler,
            &workers,
            &granted_projects(&state, team.id).await?,
        )
        .await;
    } else {
        reauth_team_workers(&scheduler, &workers).await;
    }

    audit_record(
        &state,
        Some(user.id),
        Action::TeamWorkerUpdate,
        EventOwner::default(),
        &info,
        Some(serde_json::json!({ "team_id": team.id.to_string(), "worker_id": worker_id })),
    )
    .await;

    Ok(ok_json("ok".to_string()))
}

pub async fn delete_team_worker(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path((team, worker_id)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let (team, _) = load_team(
        &state,
        &user,
        api_key.as_ref(),
        team,
        TeamAccess::Admin {
            reject_managed: false,
        },
    )
    .await?;
    let worker = find_team_worker(&state, team.id, &worker_id).await?;
    if worker.managed {
        return Err(WebError::conflict("worker is managed by server state"));
    }

    let projects = granted_projects(&state, team.id).await?;
    worker.into_active_model().delete(&state.web_db).await?;
    withdraw_team_workers(&scheduler, std::slice::from_ref(&worker_id), &projects).await;
    scheduler.connection_failures.clear(&worker_id);

    audit_record(
        &state,
        Some(user.id),
        Action::TeamWorkerDelete,
        EventOwner::default(),
        &info,
        Some(serde_json::json!({ "team_id": team.id.to_string(), "worker_id": worker_id })),
    )
    .await;

    Ok(ok_json(format!("worker '{}' unregistered", worker_id)))
}
