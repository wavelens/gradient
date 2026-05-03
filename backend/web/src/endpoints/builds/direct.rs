/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::helpers::OptionExt;
use crate::error::{WebError, WebResult};
use axum::extract::State;
use axum::{Extension, Json};
use axum_typed_multipart::{FieldData, TryFromMultipart, TypedMultipart};
use gradient_core::types::*;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
use sea_orm::{ColumnTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tempfile::NamedTempFile;
use tokio::fs;
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

/// Reject filenames from multipart uploads that would escape the upload root.
///
/// Allows nested relative paths (e.g. `src/main.rs`) but rejects absolute
/// paths, parent (`..`) / current (`.`) components, Windows path prefixes,
/// empty names, and embedded null bytes.
pub(crate) fn validate_upload_filename(filename: &str) -> WebResult<()> {
    if filename.is_empty() {
        return Err(WebError::bad_request("Empty filename"));
    }
    if filename.contains('\0') {
        return Err(WebError::bad_request("Invalid filename"));
    }
    let path = Path::new(filename);
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            _ => {
                return Err(WebError::bad_request(format!(
                    "Invalid file path: {}",
                    filename
                )));
            }
        }
    }
    Ok(())
}

#[derive(TryFromMultipart)]
pub struct DirectBuildForm {
    pub organization: String,
    pub derivation: String,
    #[form_data(limit = "unlimited")]
    pub files: Vec<FieldData<NamedTempFile>>,
}

pub async fn post_direct_build(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    TypedMultipart(form): TypedMultipart<DirectBuildForm>,
) -> WebResult<Json<BaseResponse<String>>> {
    let DirectBuildForm {
        organization,
        derivation,
        files,
    } = form;

    if files.is_empty() {
        return Err(WebError::bad_request("No files uploaded"));
    }

    let org = gradient_core::db::get_organization_by_name(Arc::clone(&state), user.id, organization.clone())
        .await?
        .or_not_found("Organization")?;

    // We'll create the DirectBuild record after the evaluation

    // Create temporary directory for files
    let temp_dir = format!("{}/uploads/{}", state.config.storage.base_path, Uuid::new_v4());
    fs::create_dir_all(&temp_dir).await.map_err(|e| {
        WebError::internal(format!("Failed to create temp directory: {}", e))
    })?;

    let temp_root = PathBuf::from(&temp_dir);
    for field in files {
        let filename = field
            .metadata
            .file_name
            .ok_or_else(|| WebError::bad_request("File field is missing a filename"))?;
        validate_upload_filename(&filename)?;
        let file_path = temp_root.join(&filename);

        if !file_path.starts_with(&temp_root) {
            return Err(WebError::bad_request(format!(
                "Invalid file path: {}",
                filename
            )));
        }

        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                WebError::internal(format!("Failed to create directory: {}", e))
            })?;
        }

        let temp_path = field.contents.into_temp_path();
        fs::copy(&temp_path, &file_path).await.map_err(|e| {
            WebError::internal(format!("Failed to write file {}: {}", filename, e))
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
        .insert(&state.web_db)
        .await
        .map_err(|e| WebError::internal(format!("Failed to create commit: {}", e)))?;

    // Create evaluation record (without project for direct builds)
    let now = gradient_core::types::now();
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
        flake_source: Set(None),
        repo_check_id: Set(None),
    };
    let evaluation = evaluation.insert(&state.web_db).await.map_err(|e| {
        WebError::internal(format!("Failed to create evaluation: {}", e))
    })?;

    // Create DirectBuild record
    let direct_build = ADirectBuild {
        id: Set(Uuid::new_v4()),
        organization: Set(org.id),
        evaluation: Set(evaluation.id),
        derivation: Set(derivation.clone()),
        repository_path: Set(temp_dir.clone()),
        created_by: Set(user.id),
        created_at: Set(gradient_core::types::now()),
    };
    direct_build.insert(&state.web_db).await.map_err(|e| {
        WebError::internal(format!("Failed to create direct build record: {}", e))
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
        .all(&state.web_db)
        .await?;

    let mut all_direct_builds = Vec::new();

    for org in organizations {
        // Get recent direct builds for this organization
        let direct_builds = EDirectBuild::find()
            .filter(CDirectBuild::Organization.eq(org.id))
            .order_by_desc(CDirectBuild::CreatedAt)
            .limit(10)
            .all(&state.web_db)
            .await?;

        for db in direct_builds {
            // Get evaluation info
            if let Ok(Some(evaluation)) =
                EEvaluation::find_by_id(db.evaluation).one(&state.web_db).await
            {
                // Get a build from this evaluation to check status
                let build_status = if let Ok(Some(build)) = EBuild::find()
                    .filter(CBuild::Evaluation.eq(evaluation.id))
                    .one(&state.web_db)
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

#[cfg(test)]
mod tests {
    use super::validate_upload_filename;

    #[test]
    fn accepts_simple_filenames() {
        assert!(validate_upload_filename("flake.nix").is_ok());
        assert!(validate_upload_filename("default.nix").is_ok());
        assert!(validate_upload_filename("src/main.rs").is_ok());
        assert!(validate_upload_filename("a/b/c/d.txt").is_ok());
    }

    #[test]
    fn rejects_empty() {
        assert!(validate_upload_filename("").is_err());
    }

    #[test]
    fn rejects_parent_traversal() {
        assert!(validate_upload_filename("..").is_err());
        assert!(validate_upload_filename("../etc/passwd").is_err());
        assert!(validate_upload_filename("../../../../../etc/cron.d/owned").is_err());
        assert!(validate_upload_filename("foo/../../bar").is_err());
        assert!(validate_upload_filename("foo/..").is_err());
    }

    #[test]
    fn rejects_absolute_paths() {
        assert!(validate_upload_filename("/etc/passwd").is_err());
        assert!(validate_upload_filename("/").is_err());
    }

    #[test]
    fn rejects_current_dir_components() {
        assert!(validate_upload_filename(".").is_err());
        assert!(validate_upload_filename("./foo").is_err());
    }

    #[test]
    fn rejects_null_bytes() {
        assert!(validate_upload_filename("foo\0bar").is_err());
    }
}
