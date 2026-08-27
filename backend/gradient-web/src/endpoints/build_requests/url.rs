/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `POST /build-requests/url` - queue a build request against a remote
//! repository instead of an uploaded source tree (#564).
//!
//! The upload flows exist so `gradient build` can ship a dirty working tree. A
//! deployment tool already has the source published at a URL and only needs
//! Gradient to evaluate one attribute of it, so there is nothing to upload:
//! `repository_url_to_nix` turns the URL and revision into exactly the source
//! string an evaluation carries, and the worker fetches it directly.
//!
//! Like the upload flows this runs on the project's reserved `build-request`
//! task, so a caller never has to create a task per job.

use super::dispatch::{
    BuildRequestSource, DispatchResponse, InputOverrideBody, queue_build_request,
    validate_remote_override,
};
use crate::access::{Caller, ProjectAccess, load_project};
use crate::authorization::MaybeApiKey;
use crate::error::{ErrorCode, WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::Permission;
use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_sources::resolve_remote_ref;
use gradient_types::input::{hex_to_vec, repository_url_to_nix, vec_to_hex};
use gradient_types::{BaseResponse, MUser};
use sea_orm::TransactionTrait;
use serde::Deserialize;
use std::sync::Arc;

/// A git commit hash is 40 hex characters, so 20 bytes once decoded.
const COMMIT_HASH_BYTES: usize = 20;

#[derive(Deserialize, Debug)]
pub struct UrlRequest {
    /// Project the build runs under; supplies the cache, the workers, and the
    /// deploy key used to reach a private repository.
    pub project: String,
    pub url: String,
    /// Branch or tag to resolve. Omit both this and `rev` to take the
    /// repository's default branch.
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,
    /// Exact commit to build, 40 hex characters. Mutually exclusive with `ref`.
    #[serde(default)]
    pub rev: Option<String>,
    /// Attribute path or wildcard to evaluate. Defaults to everything.
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub input_overrides: Vec<InputOverrideBody>,
}

pub async fn post_url(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Json(body): Json<UrlRequest>,
) -> WebResult<Json<BaseResponse<DispatchResponse>>> {
    let project = load_project(
        &state.0,
        Caller::User(&user),
        api_key.as_ref(),
        body.project,
        ProjectAccess::Require {
            permission: Permission::TriggerEvaluation,
            reject_managed: false,
        },
    )
    .await?;

    if body.git_ref.is_some() && body.rev.is_some() {
        return Err(WebError::bad_request(
            "Specify at most one of `ref` and `rev`",
        ));
    }

    let url = body.url.trim();
    if url.is_empty() {
        return Err(WebError::bad_request("`url` must not be empty"));
    }

    let input_overrides: Vec<(String, String)> = body
        .input_overrides
        .into_iter()
        .map(|o| (o.input_name, o.url))
        .collect();
    for (input_name, override_url) in &input_overrides {
        validate_remote_override(input_name, override_url)?;
    }

    let (commit_hash, label) = match body.rev.as_deref() {
        Some(rev) => {
            let hash = hex_to_vec(rev)
                .ok()
                .filter(|h| h.len() == COMMIT_HASH_BYTES)
                .ok_or_else(|| WebError::bad_request("`rev` must be a 40-character hex hash"))?;
            (hash, rev.to_string())
        }
        None => {
            let git_ref = body.git_ref.as_deref();
            let hash = resolve_remote_ref(&state.db(), &project, url, git_ref)
                .await
                .map_err(|e| {
                    WebError::bad_request_with(
                        ErrorCode::REPOSITORY_UNREACHABLE,
                        format!("Failed to resolve repository state: {}", e),
                    )
                })?;
            (hash, git_ref.unwrap_or("HEAD").to_string())
        }
    };

    // Rejects local paths, and is the same shape `parse_nix_git_url` reads back
    // on the worker.
    let repository = repository_url_to_nix(url, &vec_to_hex(&commit_hash))
        .map_err(|e| WebError::bad_request(format!("Invalid repository URL: {}", e)))?;

    let tx = state.web_db.inner().begin().await?;

    let response = queue_build_request(
        &tx,
        &state.0,
        project.id,
        &user,
        BuildRequestSource {
            repository,
            commit_hash,
            commit_message: format!("Build request {url}@{label}"),
            target: body.target,
            input_overrides,
        },
    )
    .await?;

    tx.commit().await?;

    Ok(ok_json(response))
}
