/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::dispatch::{
    BuildRequestSource, DispatchResponse, InputOverrideBody, queue_build_request,
};
use super::refusal::BuildRequestRefusal;
use crate::access::{Caller, ProjectAccess, load_project};
use crate::authorization::MaybeApiKey;
use crate::error::WebResult;
use crate::helpers::ok_json;
use crate::permissions::Permission;
use crate::refusal::SharedRefusal;
use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_sources::resolve_remote_ref;
use gradient_types::input::{
    check_flake_input_override, hex_to_vec, repository_url_to_nix, vec_to_hex,
};
use gradient_types::{BaseResponse, MUser};
use sea_orm::TransactionTrait;
use serde::Deserialize;
use std::sync::Arc;

const COMMIT_HASH_BYTES: usize = 20;

#[derive(Deserialize, Debug)]
pub struct UrlRequest {
    pub project: String,
    pub url: String,
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub rev: Option<String>,
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
        &state.0.db(),
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
        return Err(BuildRequestRefusal::RefAndRevTogether.into());
    }

    let url = body.url.trim();
    if url.is_empty() {
        return Err(SharedRefusal::EmptyField("url").into());
    }

    let input_overrides: Vec<(String, String)> = body
        .input_overrides
        .into_iter()
        .map(|o| (o.input_name, o.url))
        .collect();
    for (input_name, override_url) in &input_overrides {
        check_flake_input_override(input_name, override_url)?;
    }

    let (commit_hash, label) = match body.rev.as_deref() {
        Some(rev) => {
            let hash = hex_to_vec(rev)
                .ok()
                .filter(|h| h.len() == COMMIT_HASH_BYTES)
                .ok_or(BuildRequestRefusal::InvalidRev)?;
            (hash, rev.to_string())
        }
        None => {
            let git_ref = body.git_ref.as_deref();
            let hash = resolve_remote_ref(&state.db(), &project, url, git_ref)
                .await
                .map_err(|e| SharedRefusal::RepositoryStateFetchFailed {
                    reason: e.to_string(),
                })?;
            (hash, git_ref.unwrap_or("HEAD").to_string())
        }
    };

    let repository = repository_url_to_nix(url, &vec_to_hex(&commit_hash)).map_err(|e| {
        BuildRequestRefusal::InvalidRepositoryUrl {
            reason: e.to_string(),
        }
    })?;

    let tx = state.web_db.inner().begin().await?;

    let response = queue_build_request(
        &tx,
        &state.0.config,
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
