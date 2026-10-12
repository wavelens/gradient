/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::metrics_scope::MetricsScope;
use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::dashboard::{ActivityDay, activity};
use gradient_types::*;
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Activity {
    pub days: Vec<ActivityDay>,
}

#[utoipa::path(
    get,
    path = "/dashboard/activity",
    operation_id = "getDashboardActivity",
    tag = "dashboard",
    summary = "Daily evaluation activity of the current user",
    description = "One entry per day from 370 days ago through today, oldest first, days without evaluations included as zero.\nCounts non-PR evaluations of the projects the viewer may see; `failed` counts evaluations with status `Failed`.",
    responses((status = OK, description = "Daily evaluation counts", body = Activity), WebError)
)]
pub async fn get_activity(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Activity>>> {
    let scope = MetricsScope::resolve(&state.web_db, &Some(user)).await?;
    let filter = scope.project_in_list();
    Ok(ok_json(Activity {
        days: activity(&state.web_db, filter.as_deref()).await?,
    }))
}
