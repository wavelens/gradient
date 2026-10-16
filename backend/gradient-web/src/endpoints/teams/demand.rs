/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::api_spec::TeamPath;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::scope::{Admin, TeamScope};
use axum::Json;
use axum::extract::Query;
use gradient_db::WebDb;
use gradient_db::teams::demand::TeamDemand;
use gradient_types::*;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize, Debug, Default, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DemandQuery {
    pub projects: Option<String>,
}

#[utoipa::path(
    get,
    path = "/teams/{team}/workers/demand",
    operation_id = "getTeamWorkerDemand",
    tag = "teams",
    summary = "Read missing worker capabilities",
    description = "Needs Admin in the team. Sums the waiting evaluations of projects granting the team its workers that no connected worker eligible for the project can take: builds per system and features, plus fetch and evaluation gaps. `projects` takes comma-separated project names or IDs and limits the sum to those granting projects.",
    params(TeamPath, DemandQuery),
    responses((status = OK, description = "OK", body = TeamDemand), WebError)
)]
pub async fn get_team_worker_demand(
    TeamScope { team, db, .. }: TeamScope<Admin>,
    Query(query): Query<DemandQuery>,
) -> WebResult<Json<BaseResponse<TeamDemand>>> {
    let projects = match query.projects.as_deref() {
        Some(list) => Some(resolve_projects(&db.web_db, list).await?),
        None => None,
    };
    let demand =
        gradient_db::teams::demand::team_demand(&db.web_db, team.id, projects.as_deref()).await?;
    Ok(ok_json(demand))
}

async fn resolve_projects(db: &WebDb, list: &str) -> Result<Vec<ProjectId>, WebError> {
    let mut ids = Vec::new();
    for entry in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        if let Ok(id) = entry.parse::<Uuid>() {
            ids.push(ProjectId::new(id));
        } else if let Some(project) = gradient_db::projects::by_name(db, entry).await? {
            ids.push(project.id);
        }
    }
    Ok(ids)
}
