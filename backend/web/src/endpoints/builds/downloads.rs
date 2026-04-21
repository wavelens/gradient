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
use core::storage::nar_extract::{ExtractError, extract_file_from_nar_bytes};
use core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use super::BuildAccessContext;
use crate::endpoints::content_type_for_filename;

// ── Hydra build-product helpers ───────────────────────────────────────────────

/// Returns the store-path hash (first component, before the first `-`) for
/// `/nix/store/<hash>-<name>`. Empty string on malformed input.
fn store_path_hash(output: &str) -> &str {
    output
        .strip_prefix("/nix/store/")
        .unwrap_or(output)
        .split('-')
        .next()
        .unwrap_or("")
}

/// Strip `/nix/store/<hash>-<name>/` prefix from a product line path, returning
/// the path relative to the output's NAR root.
fn relative_in_output(full: &str, output_root: &str) -> String {
    let prefix = format!("{}/", output_root);
    full.strip_prefix(&prefix)
        .map(str::to_owned)
        .unwrap_or_else(|| full.trim_start_matches('/').to_owned())
}

/// Query `build_product` rows for a set of derivation output IDs and return them
/// as the local [`BuildProduct`] type.
async fn collect_build_products(
    state: &Arc<ServerState>,
    _build_id: Uuid,
    build_outputs: Vec<MDerivationOutput>,
) -> Vec<BuildProduct> {
    let output_ids: Vec<Uuid> = build_outputs.iter().map(|o| o.id).collect();
    if output_ids.is_empty() {
        return Vec::new();
    }
    let rows = match EBuildProduct::find()
        .filter(CBuildProduct::DerivationOutput.is_in(output_ids))
        .all(&state.db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "failed to query build_product rows");
            return Vec::new();
        }
    };
    rows.into_iter()
        .map(|r| BuildProduct {
            file_type: r.file_type,
            name: r.name,
            path: r.path,
            size: r.size.map(|s| s as u64),
        })
        .collect()
}

/// Look up `build_product` rows for the given outputs, find the one whose
/// `name` matches `filename`, and stream its bytes from `nar_storage`.
///
/// Returns `None` when no matching product is found.
async fn find_and_serve_file(
    state: &Arc<ServerState>,
    build_id: Uuid,
    build_outputs: Vec<MDerivationOutput>,
    filename: &str,
) -> WebResult<Option<Response>> {
    let output_ids: Vec<Uuid> = build_outputs.iter().map(|o| o.id).collect();
    if output_ids.is_empty() {
        return Ok(None);
    }

    let rows = match EBuildProduct::find()
        .filter(CBuildProduct::DerivationOutput.is_in(output_ids))
        .all(&state.db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(%build_id, error = %e, "failed to query build_product rows for download");
            return Ok(None);
        }
    };

    for product in rows {
        // Match by exact name or by basename of path.
        let product_name = &product.name;
        let path_basename = std::path::Path::new(&product.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if product_name != filename && path_basename != filename {
            continue;
        }

        tracing::debug!(%build_id, %filename, product_path = %product.path, "Found matching build_product, fetching from NAR");

        // Find the output that owns this product.
        let output = build_outputs
            .iter()
            .find(|o| o.id == product.derivation_output);
        let output_root = match output {
            Some(o) => &o.output,
            None => {
                tracing::warn!(%build_id, %filename, "build_product references unknown output");
                continue;
            }
        };

        let hash = store_path_hash(output_root);
        let rel = relative_in_output(&product.path, output_root);

        let compressed = match state.nar_storage.get(hash).await {
            Ok(Some(b)) => b,
            Ok(None) => {
                tracing::warn!(%build_id, %filename, hash, "NAR not found in nar_storage");
                continue;
            }
            Err(e) => {
                tracing::error!(%build_id, %filename, hash, error = %e, "Failed to fetch NAR");
                return Err(WebError::InternalServerError(
                    "Failed to fetch NAR".to_string(),
                ));
            }
        };

        match extract_file_from_nar_bytes(compressed, &rel).await {
            Ok(extracted) => {
                tracing::info!(%build_id, %filename, file_size = extracted.contents.len(), "Successfully extracted file for download");
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
                        extracted.contents,
                    )
                        .into_response(),
                ));
            }
            Err(ExtractError::NotFound) => {
                tracing::debug!(%build_id, %filename, %rel, "File not found in NAR, trying next product");
                continue;
            }
            Err(e) => {
                tracing::error!(%build_id, %filename, %rel, error = %e, "Failed to extract file from NAR");
                return Err(WebError::InternalServerError(
                    "Failed to extract file from NAR".to_string(),
                ));
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_path_hash_extracts_hash() {
        assert_eq!(
            store_path_hash("/nix/store/abc123def456-hello-2.12"),
            "abc123def456"
        );
    }

    #[test]
    fn store_path_hash_without_prefix() {
        assert_eq!(store_path_hash("abc123-name"), "abc123");
    }

    #[test]
    fn store_path_hash_empty_on_malformed() {
        assert_eq!(store_path_hash(""), "");
    }

    #[test]
    fn relative_in_output_strips_prefix() {
        let full = "/nix/store/abc123-pkg/image.iso";
        let root = "/nix/store/abc123-pkg";
        assert_eq!(relative_in_output(full, root), "image.iso");
    }

    #[test]
    fn relative_in_output_nested() {
        let full = "/nix/store/abc123-pkg/subdir/image.iso";
        let root = "/nix/store/abc123-pkg";
        assert_eq!(relative_in_output(full, root), "subdir/image.iso");
    }

    #[test]
    fn relative_in_output_fallback_on_no_prefix() {
        let full = "/other/path/image.iso";
        let root = "/nix/store/abc123-pkg";
        assert_eq!(relative_in_output(full, root), "other/path/image.iso");
    }
}
