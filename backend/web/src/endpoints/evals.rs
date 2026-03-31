/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::endpoints::user_is_org_member;
use crate::error::{WebError, WebResult};
use async_stream::stream;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use axum_streams::StreamBodyAs;
use builder::scheduler::abort_evaluation;
use core::types::*;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::error;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeEvaluationRequest {
    pub method: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildItem {
    pub id: Uuid,
    pub name: String,
    pub status: String,
    pub has_artefacts: bool,
}

#[derive(Serialize, Debug)]
pub struct EvaluationResponse {
    pub id: Uuid,
    pub project: Option<Uuid>,
    pub project_name: Option<String>,
    pub repository: String,
    pub commit: Uuid,
    pub wildcard: String,
    pub status: entity::evaluation::EvaluationStatus,
    pub previous: Option<Uuid>,
    pub next: Option<Uuid>,
    pub created_at: chrono::NaiveDateTime,
    pub error: Option<String>,
}

/// `/nix/store/hash-name-version.drv` → `name-version`
fn drv_display_name(path: &str) -> String {
    let filename = path.rsplit('/').next().unwrap_or(path);
    let after_hash = filename.split_once('-').map(|x| x.1).unwrap_or(filename);
    after_hash
        .strip_suffix(".drv")
        .unwrap_or(after_hash)
        .to_string()
}

pub async fn get_evaluation(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(evaluation_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<EvaluationResponse>>> {
    let evaluation = EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Evaluation"))?;

    let (organization_id, project_name) = if let Some(project_id) = evaluation.project {
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!(
                    "Project {} not found for evaluation {}",
                    project_id,
                    evaluation_id
                );
                WebError::InternalServerError("Evaluation data inconsistency".to_string())
            })?;
        let name = project.name.clone();
        (project.organization, Some(name))
    } else {
        let org_id = EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!("DirectBuild not found for evaluation {}", evaluation_id);
                WebError::InternalServerError("Direct build data inconsistency".to_string())
            })?
            .organization;
        (org_id, None)
    };
    let organization = EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!("Organization {} not found", organization_id);
            WebError::InternalServerError("Organization data inconsistency".to_string())
        })?;

    let can_access = if organization.public {
        true
    } else {
        match &maybe_user {
            Some(user) => user_is_org_member(&state, user.id, organization.id).await?,
            None => false,
        }
    };
    if !can_access {
        return Err(WebError::not_found("Evaluation"));
    }

    let res = BaseResponse {
        error: false,
        message: EvaluationResponse {
            id: evaluation.id,
            project: evaluation.project,
            project_name,
            repository: evaluation.repository,
            commit: evaluation.commit,
            wildcard: evaluation.wildcard,
            status: evaluation.status,
            previous: evaluation.previous,
            next: evaluation.next,
            created_at: evaluation.created_at,
            error: evaluation.error,
        },
    };

    Ok(Json(res))
}

pub async fn post_evaluation(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(evaluation_id): Path<Uuid>,
    Json(body): Json<MakeEvaluationRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let evaluation = EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Evaluation"))?;

    let organization_id = if let Some(project_id) = evaluation.project {
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!(
                    "Project {} not found for evaluation {}",
                    project_id,
                    evaluation_id
                );
                WebError::InternalServerError("Evaluation data inconsistency".to_string())
            })?;
        project.organization
    } else {
        EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!("DirectBuild not found for evaluation {}", evaluation_id);
                WebError::InternalServerError("Direct build data inconsistency".to_string())
            })?
            .organization
    };
    let organization = EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!("Organization {} not found", organization_id);
            WebError::InternalServerError("Organization data inconsistency".to_string())
        })?;

    if !user_is_org_member(&state, user.id, organization.id).await? {
        return Err(WebError::not_found("Evaluation"));
    }

    if body.method == "abort" {
        abort_evaluation(Arc::clone(&state), evaluation).await;
    }

    let res = BaseResponse {
        error: false,
        message: "Success".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_evaluation_builds(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(evaluation_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<Vec<BuildItem>>>> {
    let evaluation = EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Evaluation"))?;

    let organization_id = if let Some(project_id) = evaluation.project {
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!(
                    "Project {} not found for evaluation {}",
                    project_id,
                    evaluation_id
                );
                WebError::InternalServerError("Evaluation data inconsistency".to_string())
            })?;
        project.organization
    } else {
        EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!("DirectBuild not found for evaluation {}", evaluation_id);
                WebError::InternalServerError("Direct build data inconsistency".to_string())
            })?
            .organization
    };

    let organization = EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!("Organization {} not found", organization_id);
            WebError::InternalServerError("Organization data inconsistency".to_string())
        })?;

    let can_access = if organization.public {
        true
    } else {
        match &maybe_user {
            Some(user) => user_is_org_member(&state, user.id, organization.id).await?,
            None => false,
        }
    };
    if !can_access {
        return Err(WebError::not_found("Evaluation"));
    }

    let builds = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation.id))
        .all(&state.db)
        .await?;

    let build_ids: Vec<Uuid> = builds.iter().map(|b| b.id).collect();

    let has_artefacts_map: HashMap<Uuid, bool> = if build_ids.is_empty() {
        HashMap::new()
    } else {
        EBuildOutput::find()
            .filter(CBuildOutput::Build.is_in(build_ids))
            .filter(CBuildOutput::HasArtefacts.eq(true))
            .all(&state.db)
            .await?
            .into_iter()
            .map(|o| (o.build, true))
            .collect()
    };

    let builds: Vec<BuildItem> = builds
        .iter()
        .filter(|b| b.derivation_path.ends_with(".drv"))
        .map(|b| BuildItem {
            id: b.id,
            name: b.derivation_path.clone(),
            status: format!("{:?}", b.status),
            has_artefacts: *has_artefacts_map.get(&b.id).unwrap_or(&false),
        })
        .collect();

    let res = BaseResponse {
        error: false,
        message: builds,
    };

    Ok(Json(res))
}

pub async fn post_evaluation_builds(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(evaluation_id): Path<Uuid>,
) -> Result<StreamBodyAs<'static>, WebError> {
    let evaluation = EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Evaluation"))?;

    let organization_id = if let Some(project_id) = evaluation.project {
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!(
                    "Project {} not found for evaluation {}",
                    project_id,
                    evaluation_id
                );
                WebError::InternalServerError("Evaluation data inconsistency".to_string())
            })?;
        project.organization
    } else {
        EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!("DirectBuild not found for evaluation {}", evaluation_id);
                WebError::InternalServerError("Direct build data inconsistency".to_string())
            })?
            .organization
    };
    let organization = EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!("Organization {} not found", organization_id);
            WebError::InternalServerError("Organization data inconsistency".to_string())
        })?;

    // TODO: Check if user is in organization
    if !user_is_org_member(&state, user.id, organization.id).await? {
        return Err(WebError::not_found("Evaluation"));
    }

    let condition = Condition::all()
        .add(CBuild::Evaluation.eq(evaluation.id))
        .add(CBuild::Status.eq(entity::build::BuildStatus::Building));

    let stream = stream! {
        let mut last_logs: HashMap<Uuid, usize> = HashMap::new();

        let past_builds = match EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation.id))
            .all(&state.db)
            .await
        {
            Ok(builds) => builds,
            Err(e) => {
                error!(error = %e, "Failed to query past builds");
                return;
            }
        };

        for build in past_builds {
            let name = drv_display_name(&build.derivation_path);
            let log = state.log_storage.read(build.id).await.unwrap_or_default();
            last_logs.insert(build.id, log.len());

            // TODO: Chunkify past log
            yield log
                .split("\n")
                .map(|l| format!("{}> {}", name, l))
                .collect::<Vec<String>>()
                .join("\n");
        }

        loop {
            let builds = match EBuild::find()
                .filter(condition.clone())
                .all(&state.db)
                .await {
                Ok(b) => b,
                Err(e) => {
                    error!(error = %e, "Failed to query builds");
                    break;
                }
            };

            if builds.is_empty() {
                let all_builds = match EBuild::find()
                    .filter(
                        Condition::all()
                            .add(CBuild::Evaluation.eq(evaluation.id))
                            .add(
                                Condition::any()
                                    .add(CBuild::Status.eq(entity::build::BuildStatus::Building))
                                    .add(CBuild::Status.eq(entity::build::BuildStatus::Queued)),
                            ),
                    )
                    .one(&state.db)
                    .await {
                    Ok(b) => b,
                    Err(e) => {
                        error!(error = %e, "Failed to query all builds");
                        break;
                    }
                };

                if all_builds.is_none() {
                    yield "".to_string();
                    break;
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                continue;
            }

            for build in builds {
                let name = drv_display_name(&build.derivation_path);
                let log = state.log_storage.read(build.id).await.unwrap_or_default();
                let last_offset = *last_logs.get(&build.id).unwrap_or(&0);
                let log_new = log[last_offset..].to_string();

                if !log_new.is_empty() {
                    last_logs.insert(build.id, log.len());
                    yield log_new
                        .split("\n")
                        .map(|l| format!("{}> {}", name, l))
                        .collect::<Vec<String>>()
                        .join("\n");
                } else {
                    last_logs.entry(build.id).or_insert(0);
                }
            }
        }
    };

    Ok(StreamBodyAs::json_nl(stream))
}
