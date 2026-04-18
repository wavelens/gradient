/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeUser, decode_download_token, encode_download_token};
use crate::error::{WebError, WebResult};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::fs;
use uuid::Uuid;

use super::BuildAccessContext;
use crate::endpoints::{content_type_for_filename, parse_hydra_product_line};

// ── Hydra build-product helpers ───────────────────────────────────────────────

/// Ensure each output path is realised, then scan `hydra-build-products` and
/// return every listed product.
async fn collect_build_products(
    state: &Arc<ServerState>,
    build_id: Uuid,
    build_outputs: Vec<MDerivationOutput>,
) -> Vec<BuildProduct> {
    let mut products = Vec::new();
    for output in build_outputs {
        if let Err(e) = state.web_nix_store.ensure_path(output.output.clone()).await {
            tracing::warn!(
                %build_id, output_path = %output.output,
                error = %format!("{:#}", e),
                "Failed to ensure output path is realised"
            );
        }
        let hydra_path = format!("{}/nix-support/hydra-build-products", output.output);
        tracing::debug!(%build_id, output_path = %output.output, %hydra_path,
            "Checking for hydra-build-products file in get_build_downloads");
        if let Ok(content) = fs::read_to_string(&hydra_path).await {
            tracing::debug!(%build_id, %content, "Found hydra-build-products content");
            for line in content.lines() {
                if let Some((file_type, file_path)) = parse_hydra_product_line(line) {
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
            tracing::debug!(%build_id, %hydra_path, "hydra-build-products file not found or unreadable");
        }
    }
    products
}

/// Ensure each output path is realised, scan `hydra-build-products`, and stream
/// the first output whose filename matches `filename`.
///
/// Returns `None` when no matching file is found.
async fn find_and_serve_file(
    state: &Arc<ServerState>,
    build_id: Uuid,
    build_outputs: Vec<MDerivationOutput>,
    filename: &str,
) -> WebResult<Option<Response>> {
    for output in build_outputs {
        if let Err(e) = state.web_nix_store.ensure_path(output.output.clone()).await {
            tracing::warn!(
                %build_id, output_path = %output.output,
                error = %format!("{:#}", e),
                "Failed to ensure output path is realised"
            );
        }
        let hydra_path = format!("{}/nix-support/hydra-build-products", output.output);
        tracing::debug!(%build_id, %filename, output_path = %output.output, %hydra_path,
            "Checking for hydra-build-products file");
        let Ok(content) = fs::read_to_string(&hydra_path).await else {
            tracing::debug!(%build_id, %filename, %hydra_path, "hydra-build-products file not found or unreadable");
            continue;
        };
        tracing::debug!(%build_id, %filename, %content, "Found hydra-build-products content");
        for line in content.lines() {
            let Some((_, file_path)) = parse_hydra_product_line(line) else {
                continue;
            };
            let file_name = std::path::Path::new(&file_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            tracing::debug!(%build_id, requested_filename = %filename, found_file_name = %file_name, %file_path, "Comparing filenames");
            if file_name != filename {
                continue;
            }
            tracing::debug!(%build_id, %filename, %file_path, "Found matching file");
            let contents = fs::read(&file_path).await.map_err(|e| {
                tracing::error!(%build_id, %filename, %file_path, error = %e, "Failed to read file");
                WebError::InternalServerError("Failed to read file".to_string())
            })?;
            tracing::info!(%build_id, %filename, file_size = contents.len(), "Successfully read file for download");
            return Ok(Some(
                (
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, content_type_for_filename(filename)),
                        (
                            header::CONTENT_DISPOSITION,
                            &format!("attachment; filename=\"{}\"", filename),
                        ),
                    ],
                    contents,
                )
                    .into_response(),
            ));
        }
    }
    Ok(None)
}

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
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user).await?;

    let build_outputs = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(ctx.build.derivation))
        .all(&state.db)
        .await?;

    let products = collect_build_products(&state, build_id, build_outputs).await;

    Ok(Json(BaseResponse {
        error: false,
        message: products,
    }))
}

pub async fn get_build_download_token(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(build_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<String>>> {
    BuildAccessContext::load(&state, build_id, &Some(user)).await?;

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
    let ctx = BuildAccessContext::load_unguarded(&state, build_id).await?;

    if let Some(token_str) = query.token {
        let claims = decode_download_token(State(Arc::clone(&state)), token_str)
            .await
            .map_err(|_| WebError::Unauthorized("Invalid download token".to_string()))?;
        if claims.build_id != build_id {
            return Err(WebError::not_found("Build"));
        }
    } else if !ctx.organization.public {
        match maybe_user {
            Some(user) => {
                use crate::endpoints::user_is_org_member;
                if !user_is_org_member(&state, user.id, ctx.organization.id).await? {
                    return Err(WebError::not_found("Build"));
                }
            }
            None => return Err(WebError::Unauthorized("Authorization required".to_string())),
        }
    }

    let build_outputs = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(ctx.build.derivation))
        .all(&state.db)
        .await?;

    tracing::debug!(
        %build_id,
        output_count = build_outputs.len(),
        "Found build outputs for download"
    );

    match find_and_serve_file(&state, build_id, build_outputs, &filename).await? {
        Some(response) => Ok(response),
        None => Err(WebError::not_found("File")),
    }
}
