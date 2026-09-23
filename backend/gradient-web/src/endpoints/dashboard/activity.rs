/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::WebResult;
use crate::helpers::ok_json;
use crate::metrics_scope::MetricsScope;
use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::dashboard::{ActivityDay, activity};
use gradient_types::*;
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct Activity {
    pub days: Vec<ActivityDay>,
}

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
