/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{CacheContext, cache_client_ip};
use crate::client_ip::OptionalPeer;
use crate::error::{WebError, WebResult};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use gradient_entity::build::BuildStatus;
use gradient_sources::parse_drv_hash_name;
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;

pub async fn log(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path((cache, drv)): Path<(String, String)>,
) -> WebResult<Response> {
    let client_ip = cache_client_ip(&state, &headers, peer);
    let ctx = CacheContext::load(&state, &headers, client_ip, cache).await?;

    let Ok((drv_hash, drv_name)) = parse_drv_hash_name(&drv) else {
        return Err(WebError::not_found("Log"));
    };

    let Some(derivation_row) = EDerivation::find()
        .filter(CDerivation::Hash.eq(drv_hash))
        .filter(CDerivation::Name.eq(drv_name))
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

    let Some(anchor) = EDerivationBuild::find()
        .filter(CDerivationBuild::Derivation.eq(derivation_row.id))
        .filter(CDerivationBuild::Status.eq(BuildStatus::Completed))
        .one(&state.web_db)
        .await?
    else {
        return Err(WebError::not_found("Log"));
    };

    let body = match gradient_db::latest_attempt_id(&state.web_db, anchor.id).await? {
        Some(key) => state.log_storage.read(key).await.unwrap_or_default(),
        None => String::new(),
    };

    Response::builder()
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )
        .body(Body::from(body))
        .map_err(|e| WebError::internal(format!("Failed to build response: {}", e)))
}
