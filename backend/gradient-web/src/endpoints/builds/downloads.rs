/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::refusal::BuildReadRefusal;
use crate::api_spec::Binary;
use crate::authorization::{MaybeApiKey, MaybeUser, decode_download_token, encode_download_token};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::resource::Resource;
use crate::store_paths::original_text;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_storage::NarStore;
use gradient_storage::nar_extract::{
    ExtractError, Extracted, extract_path_from_reader, nar_reader_from_stream,
};
use gradient_types::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::BuildAccessContext;
use crate::endpoints::{archive_headers, build_product_headers};

fn relative_in_output(full: &str, output_root: &str) -> String {
    let prefix = format!("{}/", output_root);
    full.strip_prefix(&prefix)
        .map(str::to_owned)
        .unwrap_or_else(|| full.trim_start_matches('/').to_owned())
}

async fn collect_build_products(
    db: &WebDb,
    _build_id: BuildJobId,
    build_outputs: Vec<MDerivationOutput>,
) -> Vec<BuildProduct> {
    let output_ids: Vec<DerivationOutputId> = build_outputs.iter().map(|o| o.id).collect();
    if output_ids.is_empty() {
        return Vec::new();
    }

    let rows = match gradient_db::build_products::of_outputs(db, &output_ids).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "failed to query build_product rows");
            return Vec::new();
        }
    };

    rows.into_iter().map(BuildProduct::from).collect()
}

async fn find_and_serve_file(
    db: &WebDb,
    nar_storage: &NarStore,
    build_id: BuildJobId,
    build_outputs: Vec<MDerivationOutput>,
    filename: &str,
) -> WebResult<Option<Response>> {
    let output_ids: Vec<DerivationOutputId> = build_outputs.iter().map(|o| o.id).collect();
    if output_ids.is_empty() {
        return Ok(None);
    }

    let rows = match gradient_db::build_products::of_outputs(db, &output_ids).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(%build_id, error = %e, "failed to query build_product rows for download");
            return Ok(None);
        }
    };

    for product in rows
        .into_iter()
        .filter(|product| is_named(product, filename))
    {
        let Some(output) = build_outputs
            .iter()
            .find(|o| o.id == product.derivation_output)
        else {
            tracing::warn!(%build_id, %filename, "build_product references unknown output");
            continue;
        };

        let response = product_response(nar_storage, build_id, &product, output, filename).await?;
        if response.is_some() {
            return Ok(response);
        }
    }

    Ok(None)
}

fn is_named(product: &MBuildProduct, filename: &str) -> bool {
    let path_basename = std::path::Path::new(&product.path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    product.name == filename || path_basename == filename
}

async fn product_response(
    nar_storage: &NarStore,
    build_id: BuildJobId,
    product: &MBuildProduct,
    output: &MDerivationOutput,
    filename: &str,
) -> WebResult<Option<Response>> {
    tracing::debug!(%build_id, %filename, product_path = %product.path, "Found matching build_product, fetching from NAR");

    let hash = output.hash.as_str();
    let output_root = StorePath::from(output.clone()).full();
    let rel = relative_in_output(&product.path, &output_root);

    let stream = match nar_storage.get_stream(hash).await {
        Ok(Some((_size, stream))) => stream,
        Ok(None) => {
            tracing::warn!(%build_id, %filename, hash, "NAR not found in nar_storage");
            return Ok(None);
        }
        Err(e) => {
            tracing::error!(%build_id, %filename, hash, error = %e, "Failed to fetch NAR");
            return Err(anyhow::anyhow!("Failed to fetch NAR").into());
        }
    };

    match extract_path_from_reader(nar_reader_from_stream(stream), &rel).await {
        Ok(Extracted::File { contents, .. }) => {
            tracing::info!(%build_id, %filename, file_size = contents.len(), "Successfully extracted file for download");
            let headers = build_product_headers(filename, &product.subtype);

            Ok(Some((StatusCode::OK, headers, contents).into_response()))
        }
        Ok(Extracted::Directory { tar_zst }) => {
            tracing::info!(%build_id, %filename, archive_size = tar_zst.len(), "Successfully archived directory for download");
            let headers = archive_headers(&format!("{}.tar.zst", filename));

            Ok(Some((StatusCode::OK, headers, tar_zst).into_response()))
        }
        Err(ExtractError::NotFound) => {
            tracing::debug!(%build_id, %filename, %rel, "Path not found in NAR, trying next product");
            Ok(None)
        }
        Err(e) => {
            tracing::error!(%build_id, %filename, %rel, error = %e, "Failed to extract path from NAR");
            Err(anyhow::anyhow!("Failed to extract path from NAR").into())
        }
    }
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct BuildProduct {
    pub file_type: String,
    pub subtype: String,
    pub name: String,
    pub path: String,
    pub size: Option<u64>,
}

impl From<MBuildProduct> for BuildProduct {
    fn from(product: MBuildProduct) -> Self {
        Self {
            file_type: product.file_type,
            subtype: product.subtype,
            name: product.name,
            path: product.path,
            size: product.size.map(|s| s as u64),
        }
    }
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DownloadQuery {
    token: Option<String>,
}

#[utoipa::path(
    get,
    path = "/builds/{build}/downloads",
    operation_id = "getBuildDownloads",
    tag = "builds",
    summary = "List build artefacts",
    description = "Returns downloadable artefacts declared in the build's\n`nix-support/hydra-build-products` file. Each entry has a file type,\nfilename, Nix store path, and file size in bytes.\n\nAccessible without authentication when the owning project is public.\n\n**Access:** Accessible to members of any project whose evaluation references this derivation (a derivation is built once and shared across projects via cache reachability). See `docs/src/concepts/overview.md#shared-builds`.",
    params(("build" = BuildJobId, Path)),
    responses((status = OK, description = "Build artefacts", body = Vec<BuildProduct>), WebError)
)]
pub async fn get_build_downloads(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<Vec<BuildProduct>>>> {
    let ctx =
        BuildAccessContext::load(&state.web_db, build_id, &maybe_user, api_key.as_ref()).await?;

    let build_outputs =
        gradient_db::derivation_outputs::of_derivation(&state.web_db, ctx.build_job.derivation)
            .await?;

    let mapper = state.store_path_mapper(MappingContext::Project(ctx.project.id));
    let products = collect_build_products(&state.web_db, build_id, build_outputs)
        .await
        .into_iter()
        .map(|product| BuildProduct {
            path: original_text(&mapper, product.path),
            ..product
        })
        .collect::<Vec<_>>();

    Ok(ok_json(products))
}

#[utoipa::path(
    get,
    path = "/builds/{build}/download-token",
    operation_id = "getBuildDownloadToken",
    tag = "builds",
    summary = "Generate a short-lived download token",
    description = "Returns a signed JWT that is valid for **1 hour** and grants access to all\nartefact downloads for the specified build. Pass it as `?token=<value>` on\n`GET /builds/{build}/download/{filename}` to share a link without exposing\nlong-lived credentials.\n\nRequires the caller to be a member of the project that owns the build.",
    params(("build" = BuildJobId, Path)),
    responses((status = OK, description = "Download token", body = String, content_type = "application/json"), WebError)
)]
pub async fn get_build_download_token(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<String>>> {
    let ctx =
        BuildAccessContext::load(&state.web_db, build_id, &Some(user), api_key.as_ref()).await?;

    let token = encode_download_token(
        &state.jwt_secret,
        ctx.build_job.derivation,
        ctx.build_job.evaluation,
    )
    .map_err(|_| WebError::failed_to_generate_token())?;

    Ok(ok_json(token))
}

async fn load_with_download_token(
    db: &WebDb,
    jwt_secret: &SecretString,
    build_id: BuildJobId,
    token: String,
) -> WebResult<BuildAccessContext> {
    let ctx = BuildAccessContext::load_unguarded(db, build_id).await?;
    let claims = decode_download_token(jwt_secret, token)
        .await
        .map_err(|_| BuildReadRefusal::InvalidDownloadToken)?;
    if claims.derivation != ctx.build_job.derivation {
        return Err(WebError::not_found(Resource::Build));
    }
    Ok(ctx)
}

#[utoipa::path(
    get,
    path = "/builds/{build}/download/{filename}",
    operation_id = "downloadBuildArtefact",
    tag = "builds",
    summary = "Download build artefact",
    description = "Streams a build artefact. When the build product is a regular file the\nresponse carries the file's contents and a `Content-Type` derived from\nthe extension. When the build product points to a directory inside the\noutput, the response is a zstd-compressed tar archive of the subtree;\n`Content-Type` is `application/zstd` and `Content-Disposition`'s\n`filename` ends in `.tar.zst`. Clients should rely on the\n`Content-Disposition` header for the suggested filename in either case.\n\nAccess is granted when **any** of the following is satisfied:\n1. The owning project is public - no credentials required.\n2. `?token=<download_token>` - a short-lived per-build token from\n   `GET /builds/{build}/download-token` (1 h validity, safe to embed in URLs).\n3. `jwt_token` cookie or `Authorization: Bearer` header and the caller is a\n   member of the owning project, or of any project with a build of the\n   same derivation.",
    params(("build" = BuildJobId, Path), ("filename" = String, Path), DownloadQuery),
    responses((status = OK, description = "File contents (regular file) or zstd-compressed tar archive (directory).", content((Binary = "application/octet-stream"), (Binary = "application/zstd"))), WebError)
)]
pub async fn get_build_download(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path((build_id, filename)): Path<(BuildJobId, String)>,
    Query(query): Query<DownloadQuery>,
) -> Result<Response, WebError> {
    let ctx = match query.token {
        Some(token) => {
            load_with_download_token(&state.web_db, &state.jwt_secret, build_id, token).await?
        }
        None => {
            BuildAccessContext::load(&state.web_db, build_id, &maybe_user, api_key.as_ref()).await?
        }
    };

    let build_outputs =
        gradient_db::derivation_outputs::of_derivation(&state.web_db, ctx.build_job.derivation)
            .await?;

    tracing::debug!(
        %build_id,
        output_count = build_outputs.len(),
        "Found build outputs for download"
    );

    match find_and_serve_file(
        &state.web_db,
        &state.nar_storage,
        build_id,
        build_outputs,
        &filename,
    )
    .await?
    {
        Some(response) => Ok(response),
        None => Err(WebError::not_found(Resource::File)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_in_output_strips_prefix() {
        let full = "/nix/store/abc123-pkg/image.iso";
        let root = "/nix/store/abc123-pkg";
        assert_eq!(relative_in_output(full, root), "image.iso");
    }

    #[test]
    fn relative_in_output_fallback_on_no_prefix() {
        let full = "/other/path/image.iso";
        let root = "/nix/store/abc123-pkg";
        assert_eq!(relative_in_output(full, root), "other/path/image.iso");
    }
}
