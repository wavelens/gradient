/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::metrics_scope::MetricsScope;
use axum::extract::{Query, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::dashboard::{ActivityDay, activity};
use gradient_types::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Activity {
    pub days: Vec<ActivityDay>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ActivityQuery {
    pub tz_offset_minutes: Option<i32>,
}

#[utoipa::path(
    get,
    path = "/dashboard/activity",
    operation_id = "getDashboardActivity",
    tag = "dashboard",
    summary = "Daily evaluation activity of the current user",
    description = "Entries run from 370 days ago through today, one per day, oldest first.\nDays without evaluations get 0. Entries count non-PR evaluations in projects that the viewer can see. `failed` is the count of evaluations with status `Failed`.\n`tz_offset_minutes` is the offset of the viewer from UTC (120 for UTC+2). The default is 0. Values outside ±840 map to the nearest limit. Dates and today follow that local time.",
    params(ActivityQuery),
    responses((status = OK, description = "Daily evaluation counts", body = Activity), WebError)
)]
pub async fn get_activity(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Query(query): Query<ActivityQuery>,
) -> WebResult<Json<BaseResponse<Activity>>> {
    let scope = MetricsScope::resolve(&state.web_db, &Some(user)).await?;
    let filter = scope.project_in_list();
    Ok(ok_json(Activity {
        days: activity(
            &state.web_db,
            filter.as_deref(),
            query.tz_offset_minutes.unwrap_or(0),
        )
        .await?,
    }))
}
