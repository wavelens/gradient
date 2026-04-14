/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use axum::extract::{Multipart, State};
use axum::{Extension, Json};
use core::types::*;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
use sea_orm::{ColumnTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct DirectBuildRequest {
    pub organization: String,
    pub derivation: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DirectBuildInfo {
    pub id: String,
    pub derivation: String,
    pub created_at: String,
    pub evaluation_id: String,
    pub status: String,
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
    let org = core::db::get_organization_by_name(Arc::clone(&state), user.id, organization.clone())
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

    // The evaluation is now Queued; the proto scheduler's dispatch loop
    // will pick it up and send it to an available worker within seconds.
    let res = BaseResponse {
        error: false,
        message: format!("Direct build started with evaluation ID: {}", evaluation.id),
    };

    Ok(Json(res))
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
