/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

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
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeEvaluationRequest {
    pub method: String,
}

pub async fn get_evaluation(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(evaluation_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<MEvaluation>>> {
    let evaluation = EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Evaluation"))?;

    let organization_id = if let Some(project_id) = evaluation.project {
        // Regular project-based evaluation
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
        // Direct build - get organization from DirectBuild record
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

    if organization.created_by != user.id {
        return Err(WebError::not_found("Evaluation"));
    }

    let res = BaseResponse {
        error: false,
        message: evaluation,
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
        // Regular project-based evaluation
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
        // Direct build - get organization from DirectBuild record
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

    if organization.created_by != user.id {
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
    Extension(user): Extension<MUser>,
    Path(evaluation_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<ListResponse>>> {
    let evaluation = EEvaluation::find_by_id(evaluation_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Evaluation"))?;

    let organization_id = if let Some(project_id) = evaluation.project {
        // Regular project-based evaluation
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
        // Direct build - get organization from DirectBuild record
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

    if organization.created_by != user.id {
        return Err(WebError::not_found("Evaluation"));
    }

    let builds = EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation.id))
        .all(&state.db)
        .await?;

    let builds: ListResponse = builds
        .iter()
        .map(|b| ListItem {
            id: b.id,
            name: b.derivation_path.clone(),
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
        // Regular project-based evaluation
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
        // Direct build - get organization from DirectBuild record
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
    if organization.created_by != user.id {
        return Err(WebError::not_found("Evaluation"));
    }

    let condition = Condition::all()
        .add(CBuild::Evaluation.eq(evaluation.id))
        .add(CBuild::Status.eq(entity::build::BuildStatus::Building));

    let stream = stream! {
        let mut last_logs: HashMap<Uuid, String> = HashMap::new();

        let past_builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation.id))
            .all(&state.db)
            .await
            .unwrap();

        for build in past_builds {
            let name = build.derivation_path.split("-").next().unwrap();
            let name = build.derivation_path.replace(format!("{}-", name).as_str(), "").replace(".drv", "");
            let log = build.log.unwrap_or("".to_string());
            last_logs.insert(build.id, log.clone());

            // TODO: Chunkify past log
            yield log
                .split("\n")
                .map(|l| format!("{}> {}", name, l))
                .collect::<Vec<String>>()
                .join("\n");
        }

        loop {
            let builds = EBuild::find()
                .filter(condition.clone())
                .all(&state.db)
                .await
                .unwrap();

            if builds.is_empty() {
                let all_builds = EBuild::find()
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
                    .await
                    .unwrap();

                if all_builds.is_none() {
                    yield "".to_string();
                    break;
                }

                std::thread::sleep(std::time::Duration::from_secs(1));
                continue;
            }

            for build in builds {
                let name = build.derivation_path.split("-").next().unwrap();
                let name = build.derivation_path.replace(format!("{}-", name).as_str(), "").replace(".drv", "");
                let log = build.log.unwrap_or("".to_string());

                if last_logs.contains_key(&build.id) {
                    let last_log = last_logs.get(&build.id).unwrap();
                    let log_new = log.replace(last_log.as_str(), "");

                    if !log_new.is_empty() {
                        last_logs.insert(build.id, log.clone());
                        yield log_new
                            .split("\n")
                            .map(|l| format!("{}> {}", name, l))
                            .collect::<Vec<String>>()
                            .join("\n");
                    }
                } else {
                    last_logs.insert(build.id, log.clone());
                }
            }
        }
    };

    Ok(StreamBodyAs::json_nl(stream))
}
