/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeUser, decode_download_token, encode_download_token};
use crate::endpoints::user_is_org_member;
use crate::error::{WebError, WebResult};
use async_stream::stream;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use axum_streams::StreamBodyAs;
use core::types::*;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
use sea_orm::{ColumnTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::time::Duration;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildWithOutputs {
    pub id: Uuid,
    pub evaluation: Uuid,
    pub status: entity::build::BuildStatus,
    pub derivation_path: String,
    pub architecture: entity::server::Architecture,
    pub server: Option<Uuid>,
    pub output: HashMap<String, String>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

pub async fn get_build(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(build_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<BuildWithOutputs>>> {
    let build = EBuild::find_by_id(build_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Build"))?;

    let evaluation = EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!(
                "Evaluation {} not found for build {}",
                build.evaluation,
                build_id
            );
            WebError::InternalServerError("Build data inconsistency".to_string())
        })?;

    let organization_id = if let Some(project_id) = evaluation.project {
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!(
                    "Project {} not found for evaluation {}",
                    project_id,
                    evaluation.id
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
                tracing::error!("DirectBuild not found for evaluation {}", evaluation.id);
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
        return Err(WebError::not_found("Build"));
    }

    let build_outputs = EBuildOutput::find()
        .filter(CBuildOutput::Build.eq(build_id))
        .all(&state.db)
        .await?;

    let mut outputs = HashMap::new();
    for output in build_outputs {
        outputs.insert(output.name, output.output);
    }

    let build_with_outputs = BuildWithOutputs {
        id: build.id,
        evaluation: build.evaluation,
        status: build.status,
        derivation_path: build.derivation_path,
        architecture: build.architecture,
        server: build.server,
        output: outputs,
        created_at: build.created_at,
        updated_at: build.updated_at,
    };

    let res = BaseResponse {
        error: false,
        message: build_with_outputs,
    };

    Ok(Json(res))
}

pub async fn get_build_log(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(build_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<String>>> {
    let build = EBuild::find_by_id(build_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Build"))?;

    let evaluation = EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!(
                "Evaluation {} not found for build {}",
                build.evaluation,
                build_id
            );
            WebError::InternalServerError("Build data inconsistency".to_string())
        })?;

    let organization_id = if let Some(project_id) = evaluation.project {
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!(
                    "Project {} not found for evaluation {}",
                    project_id,
                    evaluation.id
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
                tracing::error!("DirectBuild not found for evaluation {}", evaluation.id);
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
        return Err(WebError::not_found("Build"));
    }

    let log_key = build.log_id.unwrap_or(build_id);
    let log = state.log_storage.read(log_key).await.unwrap_or_default();
    let res = BaseResponse {
        error: false,
        message: log,
    };

    Ok(Json(res))
}

pub async fn post_build_log(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(build_id): Path<Uuid>,
) -> Result<Response, WebError> {
    let build = EBuild::find_by_id(build_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Build"))?;

    let evaluation = EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!(
                "Evaluation {} not found for build {}",
                build.evaluation,
                build_id
            );
            WebError::InternalServerError("Build data inconsistency".to_string())
        })?;
    let organization_id = if let Some(project_id) = evaluation.project {
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!(
                    "Project {} not found for evaluation {}",
                    project_id,
                    evaluation.id
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
                tracing::error!("DirectBuild not found for evaluation {}", evaluation.id);
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
        return Err(WebError::not_found("Build"));
    }

    // Capture current log length so the stream only delivers new content,
    // avoiding duplication of what the client already received via GET.
    let log_key = build.log_id.unwrap_or(build_id);
    let initial_offset = state
        .log_storage
        .read(log_key)
        .await
        .unwrap_or_default()
        .len();

    let stream = stream! {
        let mut last_offset: usize = initial_offset;
        let mut sent_any: bool = false;

        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;

            let build = match EBuild::find_by_id(build_id).one(&state.db).await {
                Ok(Some(b)) => b,
                Ok(None) => break,
                Err(_) => break,
            };

            let log = state.log_storage.read(build.log_id.unwrap_or(build_id)).await.unwrap_or_default();
            let log_new = log[last_offset..].to_string();

            if !log_new.is_empty() {
                sent_any = true;
                last_offset = log.len();
                yield log_new;
            }

            if build.status != entity::build::BuildStatus::Building {
                // One extra read: catches log lines flushed between our read
                // above and the status transition being committed.
                let final_log = state.log_storage.read(build.log_id.unwrap_or(build_id)).await.unwrap_or_default();
                let final_chunk = final_log[last_offset..].to_string();
                if !final_chunk.is_empty() {
                    sent_any = true;
                    yield final_chunk;
                }
                if !sent_any {
                    yield "".to_string();
                }
                break;
            }
        }
    };

    let mut response = StreamBodyAs::json_nl(stream).into_response();
    response
        .headers_mut()
        .insert("X-Accel-Buffering", HeaderValue::from_static("no"));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(response)
}

#[derive(Deserialize)]
pub struct DirectBuildRequest {
    pub organization: String,
    pub derivation: String,
}

pub async fn post_direct_build(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    mut multipart: Multipart,
) -> WebResult<Json<BaseResponse<String>>> {
    let mut organization = None;
    let mut derivation = None;
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();

    // Parse multipart form
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| WebError::BadRequest(format!("Failed to parse multipart: {}", e)))?
    {
        let name = field.name().unwrap_or("").to_string();

        if name == "organization" {
            organization = Some(field.text().await.map_err(|e| {
                WebError::BadRequest(format!("Failed to read organization: {}", e))
            })?);
        } else if name == "derivation" {
            derivation =
                Some(field.text().await.map_err(|e| {
                    WebError::BadRequest(format!("Failed to read derivation: {}", e))
                })?);
        } else if name.starts_with("file:") {
            let filename = match name.strip_prefix("file:") {
                Some(f) => f.to_string(),
                None => return Err(WebError::BadRequest("Invalid file field name".to_string())),
            };
            let data = field.bytes().await.map_err(|e| {
                WebError::BadRequest(format!("Failed to read file {}: {}", filename, e))
            })?;
            files.insert(filename, data.to_vec());
        }
    }

    let organization = organization
        .ok_or_else(|| WebError::BadRequest("Missing organization parameter".to_string()))?;

    let derivation = derivation
        .ok_or_else(|| WebError::BadRequest("Missing derivation parameter".to_string()))?;

    if files.is_empty() {
        return Err(WebError::BadRequest("No files uploaded".to_string()));
    }

    // Get organization
    let org =
        core::database::get_organization_by_name(Arc::clone(&state), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    // We'll create the DirectBuild record after the evaluation

    // Create temporary directory for files
    let temp_dir = format!("{}/uploads/{}", state.cli.base_path, Uuid::new_v4());
    fs::create_dir_all(&temp_dir).await.map_err(|e| {
        WebError::InternalServerError(format!("Failed to create temp directory: {}", e))
    })?;

    // Write files to temp directory
    for (filename, data) in files {
        let file_path = format!("{}/{}", temp_dir, filename);

        // Create parent directories if needed
        if let Some(parent) = std::path::Path::new(&file_path).parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                WebError::InternalServerError(format!("Failed to create directory: {}", e))
            })?;
        }

        let mut file = fs::File::create(&file_path).await.map_err(|e| {
            WebError::InternalServerError(format!("Failed to create file {}: {}", filename, e))
        })?;

        file.write_all(&data).await.map_err(|e| {
            WebError::InternalServerError(format!("Failed to write file {}: {}", filename, e))
        })?;
    }

    // Create commit record
    let commit = ACommit {
        id: Set(Uuid::new_v4()),
        message: Set("Direct build submission".to_string()),
        hash: Set(vec![0; 20]), // Dummy hash for direct builds
        author: Set(Some(user.id)),
        author_name: Set(user.name.clone()),
    };
    let commit = commit
        .insert(&state.db)
        .await
        .map_err(|e| WebError::InternalServerError(format!("Failed to create commit: {}", e)))?;

    // Create evaluation record (without project for direct builds)
    let now = chrono::Utc::now().naive_utc();
    let evaluation = AEvaluation {
        id: Set(Uuid::new_v4()),
        project: Set(None), // No project for direct builds
        repository: Set(temp_dir.clone()),
        commit: Set(commit.id),
        wildcard: Set(derivation.clone()),
        status: Set(entity::evaluation::EvaluationStatus::Queued),
        previous: Set(None),
        next: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        error: Set(None),
    };
    let evaluation = evaluation.insert(&state.db).await.map_err(|e| {
        WebError::InternalServerError(format!("Failed to create evaluation: {}", e))
    })?;

    // Create DirectBuild record
    let direct_build = ADirectBuild {
        id: Set(Uuid::new_v4()),
        organization: Set(org.id),
        evaluation: Set(evaluation.id),
        derivation: Set(derivation.clone()),
        repository_path: Set(temp_dir.clone()),
        created_by: Set(user.id),
        created_at: Set(chrono::Utc::now().naive_utc()),
    };
    direct_build.insert(&state.db).await.map_err(|e| {
        WebError::InternalServerError(format!("Failed to create direct build record: {}", e))
    })?;

    // Schedule evaluation
    builder::evaluator::evaluate_direct(Arc::clone(&state), evaluation.clone(), temp_dir)
        .await
        .map_err(|e| WebError::InternalServerError(format!("Failed to start evaluation: {}", e)))?;

    let res = BaseResponse {
        error: false,
        message: format!("Direct build started with evaluation ID: {}", evaluation.id),
    };

    Ok(Json(res))
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildProduct {
    pub file_type: String,
    pub name: String,
    pub path: String,
    pub size: Option<u64>,
}

pub async fn get_build_downloads(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(build_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<Vec<BuildProduct>>>> {
    let build = EBuild::find_by_id(build_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Build"))?;

    let evaluation = EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!(
                "Evaluation {} not found for build {}",
                build.evaluation,
                build_id
            );
            WebError::InternalServerError("Build data inconsistency".to_string())
        })?;

    let organization_id = if let Some(project_id) = evaluation.project {
        // Regular project-based build
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!(
                    "Project {} not found for evaluation {}",
                    project_id,
                    evaluation.id
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
                tracing::error!("DirectBuild not found for evaluation {}", evaluation.id);
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
        return Err(WebError::not_found("Build"));
    }

    // Get build outputs to find the nix store paths
    let build_outputs = EBuildOutput::find()
        .filter(CBuildOutput::Build.eq(build_id))
        .all(&state.db)
        .await?;

    let mut products = Vec::new();

    for output in build_outputs {
        let hydra_products_path = format!("{}/nix-support/hydra-build-products", output.output);

        tracing::debug!(
            build_id = %build_id,
            output_path = %output.output,
            hydra_products_path = %hydra_products_path,
            "Checking for hydra-build-products file in get_build_downloads"
        );

        // Check if hydra-build-products file exists
        if let Ok(content) = fs::read_to_string(&hydra_products_path).await {
            tracing::debug!(
                build_id = %build_id,
                content = %content,
                "Found hydra-build-products content in get_build_downloads"
            );

            for line in content.lines() {
                if line.trim().is_empty() {
                    continue;
                }

                // Parse line format: "file <type> <path>"
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 && parts[0] == "file" {
                    let file_type = parts[1].to_string();
                    let file_path = parts[2..].join(" ");

                    // Extract filename from path
                    let filename = std::path::Path::new(&file_path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&file_path)
                        .to_string();

                    let size = fs::metadata(&file_path).await.ok().map(|m| m.len());
                    products.push(BuildProduct {
                        file_type,
                        name: filename,
                        path: file_path,
                        size,
                    });
                }
            }
        } else {
            tracing::debug!(
                build_id = %build_id,
                hydra_products_path = %hydra_products_path,
                "hydra-build-products file not found or unreadable in get_build_downloads"
            );
        }
    }

    let res = BaseResponse {
        error: false,
        message: products,
    };

    Ok(Json(res))
}

#[derive(Deserialize)]
pub struct DownloadQuery {
    token: Option<String>,
}

pub async fn get_build_download_token(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(build_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<String>>> {
    let build = EBuild::find_by_id(build_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Build"))?;

    let evaluation = EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::InternalServerError("Build data inconsistency".to_string()))?;

    let organization_id = if let Some(project_id) = evaluation.project {
        EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                WebError::InternalServerError("Evaluation data inconsistency".to_string())
            })?
            .organization
    } else {
        EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                WebError::InternalServerError("Direct build data inconsistency".to_string())
            })?
            .organization
    };

    if !user_is_org_member(&state, user.id, organization_id).await? {
        return Err(WebError::not_found("Build"));
    }

    let token = encode_download_token(State(Arc::clone(&state)), build_id)
        .map_err(|_| WebError::failed_to_generate_token())?;

    Ok(Json(BaseResponse {
        error: false,
        message: token,
    }))
}

pub async fn get_build_download(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path((build_id, filename)): Path<(Uuid, String)>,
    Query(query): Query<DownloadQuery>,
) -> Result<Response, WebError> {
    let build = EBuild::find_by_id(build_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Build"))?;

    let evaluation = EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!(
                "Evaluation {} not found for build {}",
                build.evaluation,
                build_id
            );
            WebError::InternalServerError("Build data inconsistency".to_string())
        })?;

    let organization_id = if let Some(project_id) = evaluation.project {
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!(
                    "Project {} not found for evaluation {}",
                    project_id,
                    evaluation.id
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
                tracing::error!("DirectBuild not found for evaluation {}", evaluation.id);
                WebError::InternalServerError("Direct build data inconsistency".to_string())
            })?
            .organization
    };

    let organization = EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            WebError::InternalServerError("Organization data inconsistency".to_string())
        })?;

    if let Some(token_str) = query.token {
        let claims = decode_download_token(State(Arc::clone(&state)), token_str)
            .await
            .map_err(|_| WebError::Unauthorized("Invalid download token".to_string()))?;
        if claims.build_id != build_id {
            return Err(WebError::not_found("Build"));
        }
    } else if !organization.public {
        match maybe_user {
            Some(user) => {
                if !user_is_org_member(&state, user.id, organization_id).await? {
                    return Err(WebError::not_found("Build"));
                }
            }
            None => return Err(WebError::Unauthorized("Authorization required".to_string())),
        }
    }

    // Get build outputs to find the file
    let build_outputs = EBuildOutput::find()
        .filter(CBuildOutput::Build.eq(build_id))
        .all(&state.db)
        .await?;

    tracing::debug!(
        build_id = %build_id,
        output_count = build_outputs.len(),
        "Found build outputs for download"
    );

    for output in build_outputs {
        let hydra_products_path = format!("{}/nix-support/hydra-build-products", output.output);

        tracing::debug!(
            build_id = %build_id,
            filename = %filename,
            output_path = %output.output,
            hydra_products_path = %hydra_products_path,
            "Checking for hydra-build-products file in get_build_download"
        );

        // Check if hydra-build-products file exists
        if let Ok(content) = fs::read_to_string(&hydra_products_path).await {
            tracing::debug!(
                build_id = %build_id,
                filename = %filename,
                content = %content,
                "Found hydra-build-products content in get_build_download"
            );

            for line in content.lines() {
                if line.trim().is_empty() {
                    continue;
                }

                // Parse line format: "file <type> <path>"
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 && parts[0] == "file" {
                    let file_path = parts[2..].join(" ");

                    // Check if this file matches the requested filename
                    let file_name = std::path::Path::new(&file_path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("");

                    tracing::debug!(
                        build_id = %build_id,
                        requested_filename = %filename,
                        found_file_name = %file_name,
                        file_path = %file_path,
                        "Comparing filenames"
                    );

                    if file_name == filename {
                        tracing::debug!(
                            build_id = %build_id,
                            filename = %filename,
                            file_path = %file_path,
                            "Found matching file, attempting to read"
                        );

                        // Try to read and serve the file
                        match fs::read(&file_path).await {
                            Ok(contents) => {
                                tracing::info!(
                                    build_id = %build_id,
                                    filename = %filename,
                                    file_size = contents.len(),
                                    "Successfully read file for download"
                                );

                                // Determine content type based on file extension
                                let content_type = match std::path::Path::new(&filename)
                                    .extension()
                                    .and_then(|ext| ext.to_str())
                                {
                                    Some("tar") => "application/x-tar",
                                    Some("gz") => "application/gzip",
                                    Some("zst") => "application/zstd",
                                    Some("txt") => "text/plain",
                                    Some("json") => "application/json",
                                    Some("zip") => "application/zip",
                                    _ => "application/octet-stream",
                                };

                                return Ok((
                                    StatusCode::OK,
                                    [
                                        (header::CONTENT_TYPE, content_type),
                                        (
                                            header::CONTENT_DISPOSITION,
                                            &format!("attachment; filename=\"{}\"", filename),
                                        ),
                                    ],
                                    contents,
                                )
                                    .into_response());
                            }
                            Err(e) => {
                                tracing::error!(
                                    build_id = %build_id,
                                    filename = %filename,
                                    file_path = %file_path,
                                    error = %e,
                                    "Failed to read file"
                                );
                                return Err(WebError::InternalServerError(
                                    "Failed to read file".to_string(),
                                ));
                            }
                        }
                    }
                }
            }
        } else {
            tracing::debug!(
                build_id = %build_id,
                filename = %filename,
                hydra_products_path = %hydra_products_path,
                "hydra-build-products file not found or unreadable in get_build_download"
            );
        }
    }

    Err(WebError::not_found("File"))
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DirectBuildInfo {
    pub id: String,
    pub derivation: String,
    pub created_at: String,
    pub evaluation_id: String,
    pub status: String,
}

pub async fn get_recent_direct_builds(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Vec<DirectBuildInfo>>>> {
    // Get user's organizations
    let organizations = EOrganization::find()
        .filter(COrganization::CreatedBy.eq(user.id))
        .all(&state.db)
        .await?;

    let mut all_direct_builds = Vec::new();

    for org in organizations {
        // Get recent direct builds for this organization
        let direct_builds = EDirectBuild::find()
            .filter(CDirectBuild::Organization.eq(org.id))
            .order_by_desc(CDirectBuild::CreatedAt)
            .limit(10)
            .all(&state.db)
            .await?;

        for db in direct_builds {
            // Get evaluation info
            if let Ok(Some(evaluation)) =
                EEvaluation::find_by_id(db.evaluation).one(&state.db).await
            {
                // Get a build from this evaluation to check status
                let build_status = if let Ok(Some(build)) = EBuild::find()
                    .filter(CBuild::Evaluation.eq(evaluation.id))
                    .one(&state.db)
                    .await
                {
                    format!("{:?}", build.status)
                } else {
                    "Unknown".to_string()
                };

                all_direct_builds.push(DirectBuildInfo {
                    id: db.id.to_string(),
                    derivation: db.derivation,
                    created_at: db.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                    evaluation_id: evaluation.id.to_string(),
                    status: build_status,
                });
            }
        }
    }

    // Sort by created_at descending
    all_direct_builds.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    // Take only the most recent 20
    all_direct_builds.truncate(20);

    let res = BaseResponse {
        error: false,
        message: all_direct_builds,
    };

    Ok(Json(res))
}

// ── Dependency graph helpers ──────────────────────────────────────────────────

fn extract_drv_name(path: &str) -> String {
    let filename = path.split('/').next_back().unwrap_or(path);
    // Strip the nix store hash prefix (e.g. "abc123xyz-name.drv" → "name")
    let without_hash = filename.split_once('-').map(|x| x.1).unwrap_or(filename);
    without_hash.trim_end_matches(".drv").to_string()
}

async fn authorize_build_opt(
    state: &Arc<ServerState>,
    maybe_user: &Option<MUser>,
    build_id: Uuid,
) -> WebResult<()> {
    let build = EBuild::find_by_id(build_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Build"))?;

    let evaluation = EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::InternalServerError("Build data inconsistency".to_string()))?;

    let organization_id = if let Some(project_id) = evaluation.project {
        EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                WebError::InternalServerError("Evaluation data inconsistency".to_string())
            })?
            .organization
    } else {
        EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                WebError::InternalServerError("Direct build data inconsistency".to_string())
            })?
            .organization
    };

    let organization = EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            WebError::InternalServerError("Organization data inconsistency".to_string())
        })?;

    let can_access = if organization.public {
        true
    } else {
        match maybe_user {
            Some(user) => user_is_org_member(state, user.id, organization.id).await?,
            None => false,
        }
    };
    if !can_access {
        return Err(WebError::not_found("Build"));
    }

    Ok(())
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DependencyNode {
    pub id: Uuid,
    pub name: String,
    pub path: String,
    pub status: String,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DependencyEdge {
    pub source: Uuid,
    pub target: Uuid,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildGraph {
    pub root: Uuid,
    pub nodes: Vec<DependencyNode>,
    pub edges: Vec<DependencyEdge>,
}

/// GET /builds/{build}/dependencies — direct dependencies of a single build
pub async fn get_build_dependencies(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(build_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<Vec<DependencyNode>>>> {
    authorize_build_opt(&state, &maybe_user, build_id).await?;

    let dep_rows = EBuildDependency::find()
        .filter(CBuildDependency::Build.eq(build_id))
        .all(&state.db)
        .await?;

    let dep_ids: Vec<Uuid> = dep_rows.iter().map(|d| d.dependency).collect();

    let dep_builds = if dep_ids.is_empty() {
        vec![]
    } else {
        EBuild::find()
            .filter(CBuild::Id.is_in(dep_ids))
            .all(&state.db)
            .await?
    };

    let nodes: Vec<DependencyNode> = dep_builds
        .iter()
        .map(|b| DependencyNode {
            id: b.id,
            name: extract_drv_name(&b.derivation_path),
            path: b.derivation_path.clone(),
            status: format!("{:?}", b.status),
            created_at: b.created_at,
            updated_at: b.updated_at,
        })
        .collect();

    Ok(Json(BaseResponse {
        error: false,
        message: nodes,
    }))
}

/// GET /builds/{build}/graph — full transitive dependency graph rooted at a build
pub async fn get_build_graph(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(build_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<BuildGraph>>> {
    authorize_build_opt(&state, &maybe_user, build_id).await?;

    let mut visited: HashSet<Uuid> = HashSet::new();
    let mut nodes: Vec<DependencyNode> = Vec::new();
    let mut edges: Vec<DependencyEdge> = Vec::new();
    let mut queue: VecDeque<Vec<Uuid>> = VecDeque::new();

    visited.insert(build_id);
    queue.push_back(vec![build_id]);

    while let Some(batch) = queue.pop_front() {
        if nodes.len() >= 500 {
            break;
        }

        // Fetch all builds in this batch
        let builds = EBuild::find()
            .filter(CBuild::Id.is_in(batch.clone()))
            .all(&state.db)
            .await?;

        for build in builds {
            nodes.push(DependencyNode {
                id: build.id,
                name: extract_drv_name(&build.derivation_path),
                path: build.derivation_path.clone(),
                status: format!("{:?}", build.status),
                created_at: build.created_at,
                updated_at: build.updated_at,
            });
        }

        // Fetch all dependency edges for this batch
        let dep_rows = EBuildDependency::find()
            .filter(CBuildDependency::Build.is_in(batch))
            .all(&state.db)
            .await?;

        let mut next_batch: Vec<Uuid> = Vec::new();
        for dep in dep_rows {
            // Edge: dependency → build (dep is built before build)
            edges.push(DependencyEdge {
                source: dep.dependency,
                target: dep.build,
            });
            if !visited.contains(&dep.dependency) {
                visited.insert(dep.dependency);
                next_batch.push(dep.dependency);
            }
        }

        if !next_batch.is_empty() {
            queue.push_back(next_batch);
        }
    }

    Ok(Json(BaseResponse {
        error: false,
        message: BuildGraph {
            root: build_id,
            nodes,
            edges,
        },
    }))
}
