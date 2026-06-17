/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `POST /build-requests/source` - accepts a pre-packed source NAR (multipart
//! fields `nar`, `target`, `system`), computes the `/nix/store/<hash>-source`
//! path server-side, and finalises a build-request evaluation. The `nix`-feature
//! CLI uses this to skip the per-file blob manifest.

use super::dispatch::{DispatchResponse, finalize_build_request};
use crate::access::{Caller, OrgAccess, load_org};
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::Permission;
use axum::Extension;
use axum::Json;
use axum::extract::{Multipart, Query, State};
use gradient_core::ServerState;
use gradient_storage::source_nar::source_nar_from_bytes;
use gradient_types::{BaseResponse, MUser};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct SourceQuery {
    pub organization: String,
}

pub async fn post_source(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Query(query): Query<SourceQuery>,
    mut multipart: Multipart,
) -> WebResult<Json<BaseResponse<DispatchResponse>>> {
    let org = load_org(
        &state.0,
        Caller::User(&user),
        api_key.as_ref(),
        query.organization,
        OrgAccess::Require {
            permission: Permission::TriggerEvaluation,
            reject_managed: false,
        },
    )
    .await?;

    let mut nar_bytes: Option<Vec<u8>> = None;
    let mut target: Option<String> = None;
    let mut system: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| WebError::bad_request(format!("Invalid multipart payload: {}", e)))?
    {
        match field.name() {
            Some("nar") => {
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| WebError::bad_request(format!("Failed to read nar: {}", e)))?;
                nar_bytes = Some(data.to_vec());
            }
            Some("target") => target = field.text().await.ok(),
            Some("system") => system = field.text().await.ok(),
            _ => {}
        }
    }

    let nar_bytes = nar_bytes.ok_or_else(|| WebError::bad_request("missing `nar` field"))?;
    if nar_bytes.is_empty() {
        return Err(WebError::bad_request("empty `nar` field"));
    }

    let nar = source_nar_from_bytes(nar_bytes)
        .await
        .map_err(|e| WebError::internal(format!("Failed to read source NAR: {}", e)))?;

    let response =
        finalize_build_request(&state, org.id, &user, &nar, target, system).await?;

    Ok(ok_json(response))
}
