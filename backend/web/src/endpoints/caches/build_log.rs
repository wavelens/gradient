/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::CacheContext;
use crate::error::{WebError, WebResult};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use entity::build::BuildStatus;
use gradient_core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use std::sync::Arc;

pub async fn log(
    state: State<Arc<ServerState>>,
    headers: HeaderMap,
    Path((cache, drv)): Path<(String, String)>,
) -> WebResult<Response> {
    let ctx = CacheContext::load(&state, &headers, cache).await?;

    if !drv.ends_with(".drv") {
        return Err(WebError::not_found("Log"));
    }

    let derivation_path = format!("/nix/store/{}", drv);

    let Some(derivation_row) = EDerivation::find()
        .filter(CDerivation::DerivationPath.eq(derivation_path))
        .one(&state.web_db)
        .await?
    else {
        return Err(WebError::not_found("Log"));
    };

    let linked = ECacheDerivation::find()
        .filter(CCacheDerivation::Cache.eq(ctx.cache.id))
        .filter(CCacheDerivation::Derivation.eq(derivation_row.id))
        .one(&state.web_db)
        .await?
        .is_some();
    if !linked {
        return Err(WebError::not_found("Log"));
    }

    let Some(build) = EBuild::find()
        .filter(CBuild::Derivation.eq(derivation_row.id))
        .filter(CBuild::Status.eq(BuildStatus::Completed))
        .order_by_desc(CBuild::CreatedAt)
        .one(&state.web_db)
        .await?
    else {
        return Err(WebError::not_found("Log"));
    };

    let log_key = build.log_id.unwrap_or(build.id);
    let body = state.log_storage.read(log_key).await.unwrap_or_default();

    Response::builder()
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )
        .body(Body::from(body))
        .map_err(|e| WebError::internal(format!("Failed to build response: {}", e)))
}
