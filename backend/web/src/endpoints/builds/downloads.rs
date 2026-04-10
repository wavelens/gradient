/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeUser, decode_download_token, encode_download_token};
use crate::endpoints::user_is_org_member;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use core::types::*;
use sea_orm::EntityTrait;
use sea_orm::{ColumnTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::fs;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildProduct {
    pub file_type: String,
    pub name: String,
    pub path: String,
    pub size: Option<u64>,
}

#[derive(Deserialize)]
pub struct DownloadQuery {
    token: Option<String>,
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

    // Get derivation outputs to find the nix store paths
    let build_outputs = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(build.derivation))
        .all(&state.db)
        .await?;

    let mut products = Vec::new();

    for output in build_outputs {
        // Substituted builds may have only the metadata locally; lazily realise
        // the output path so the hydra-build-products file becomes readable.
        if let Err(e) = state.web_nix_store.ensure_path(output.output.clone()).await {
            tracing::warn!(
                build_id = %build_id,
                output_path = %output.output,
                error = %format!("{:#}", e),
                "Failed to ensure output path is realised"
            );
        }

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

    // Get derivation outputs to find the file
    let build_outputs = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(build.derivation))
        .all(&state.db)
        .await?;

    tracing::debug!(
        build_id = %build_id,
        output_count = build_outputs.len(),
        "Found build outputs for download"
    );

    for output in build_outputs {
        // Substituted builds may have only the metadata locally; lazily realise
        // the output path so the hydra-build-products file becomes readable.
        if let Err(e) = state.web_nix_store.ensure_path(output.output.clone()).await {
            tracing::warn!(
                build_id = %build_id,
                output_path = %output.output,
                error = %format!("{:#}", e),
                "Failed to ensure output path is realised"
            );
        }

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
