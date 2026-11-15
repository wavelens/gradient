/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use axum::extract::State;
use axum::{Extension, Json};
use chrono::{NaiveDate, NaiveDateTime};
use gradient_core::ServerState;
use gradient_db::yearly_instance_summary::{
    YearlyRecapStatistics, load_current_public_project_ids, load_recap_snapshot_for_year,
    recap_year_revealed_on_utc_date,
};
use gradient_entity::yearly_instance_recap_snapshot;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::ConnectionTrait;
use serde::Serialize;

use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::resource::Resource;

#[derive(Serialize, Debug, utoipa::ToSchema)]
pub struct YearlyRecapResponse {
    pub year: i32,
    pub created_at: NaiveDateTime,
    pub data_since: NaiveDate,
    pub opened: bool,
    pub statistics: YearlyRecapStatistics,
}

async fn find_yearly_recap_snapshot_for_open_reveal_window<C: ConnectionTrait>(
    db: &C,
    yearly_recap_enabled: bool,
) -> WebResult<Option<yearly_instance_recap_snapshot::Model>> {
    if !yearly_recap_enabled {
        return Ok(None);
    }
    let Some(year) = recap_year_revealed_on_utc_date(gradient_types::now().date()) else {
        return Ok(None);
    };
    Ok(load_recap_snapshot_for_year(db, year).await?)
}

#[utoipa::path(
    get,
    path = "/user/yearly-recap",
    operation_id = "getCurrentYearRecap",
    tag = "user",
    summary = "Get current Gradient Wrapped",
    description = "Return Gradient Wrapped while public. Gradient Wrapped is the yearly summary for this instance. The publication window is December 1 of the ending year to January 10 (UTC). Use `opened` to show whether the caller already opened the recap. Return `null` outside this window, with server option `yearly_recap_enabled` off, or before creation of the summary. Names of private projects, their tasks and builds and the name of the busiest worker are empty with `yearly_recap_public_projects_only`.",
    responses((status = OK, description = "The current recap, or null.", body = Option<YearlyRecapResponse>, content_type = "application/json"), WebError)
)]
pub async fn get_current_year_recap(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Option<YearlyRecapResponse>>>> {
    let Some(snapshot) = find_yearly_recap_snapshot_for_open_reveal_window(
        &state.web_db,
        state.config.server.yearly_recap_enabled,
    )
    .await?
    else {
        return Ok(ok_json(None));
    };
    let mut statistics: YearlyRecapStatistics = serde_json::from_value(snapshot.statistics)
        .map_err(|error| {
            WebError::Internal(anyhow::anyhow!(
                "Failed to read the statistics of the stored yearly recap: {error}"
            ))
        })?;
    if state.config.server.yearly_recap_public_projects_only {
        let public_project_ids =
            load_current_public_project_ids(&state.web_db, statistics.referenced_project_ids())
                .await?;
        statistics
            .remove_private_names_when_recap_is_limited_to_public_projects(&public_project_ids);
    }

    Ok(ok_json(Some(YearlyRecapResponse {
        year: snapshot.year,
        created_at: snapshot.created_at,
        data_since: snapshot.data_since,
        opened: user
            .last_opened_yearly_recap_year
            .is_some_and(|opened| opened >= snapshot.year),
        statistics,
    })))
}

#[utoipa::path(
    put,
    path = "/user/yearly-recap",
    operation_id = "putMarkCurrentYearlyRecapOpened",
    tag = "user",
    summary = "Mark Gradient Wrapped as opened",
    description = "Record that the caller opened the current Gradient Wrapped. Web clients open the summary once automatically and send this request when the user closes it. The response is `404` when no summary is revealed.",
    responses((status = OK, description = "Marked the summary as opened", body = String, content_type = "application/json"), WebError)
)]
pub async fn put_mark_current_yearly_recap_opened(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<String>>> {
    let snapshot = find_yearly_recap_snapshot_for_open_reveal_window(
        &state.web_db,
        state.config.server.yearly_recap_enabled,
    )
    .await?
    .ok_or_else(|| WebError::not_found(Resource::YearlyRecap))?;

    let mut auser: AUser = user.into();
    auser.last_opened_yearly_recap_year = Set(Some(snapshot.year));
    gradient_db::rows::update::<EUser, _>(&state.web_db, auser).await?;

    Ok(ok_json("Marked the summary as opened".to_string()))
}
