/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use std::collections::HashMap;

pub async fn active_team_worker<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
) -> Result<Option<MTeamWorker>, DbErr> {
    ETeamWorker::find()
        .filter(CTeamWorker::WorkerId.eq(worker_id))
        .filter(CTeamWorker::Active.eq(true))
        .one(db)
        .await
}

pub async fn is_team_worker<C: ConnectionTrait>(db: &C, worker_id: &str) -> Result<bool, DbErr> {
    Ok(ETeamWorker::find()
        .filter(CTeamWorker::WorkerId.eq(worker_id))
        .one(db)
        .await?
        .is_some())
}

pub async fn projects_granted_with_workers<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
) -> Result<Vec<ProjectId>, DbErr> {
    Ok(ETeamProject::find()
        .filter(CTeamProject::Team.eq(team))
        .filter(CTeamProject::IncludesWorkers.eq(true))
        .all(db)
        .await?
        .into_iter()
        .map(|grant| grant.project)
        .collect())
}

pub async fn team_workers_for_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<(String, MTeamWorker)>, DbErr> {
    let teams: Vec<TeamId> = ETeamProject::find()
        .filter(CTeamProject::Project.eq(project))
        .filter(CTeamProject::IncludesWorkers.eq(true))
        .all(db)
        .await?
        .into_iter()
        .map(|grant| grant.team)
        .collect();
    if teams.is_empty() {
        return Ok(Vec::new());
    }

    let names: HashMap<TeamId, String> = ETeam::find()
        .filter(CTeam::Id.is_in(teams.clone()))
        .all(db)
        .await?
        .into_iter()
        .map(|team| (team.id, team.name))
        .collect();

    Ok(ETeamWorker::find()
        .filter(CTeamWorker::Team.is_in(teams))
        .all(db)
        .await?
        .into_iter()
        .filter_map(|worker| names.get(&worker.team).cloned().map(|name| (name, worker)))
        .collect())
}

pub async fn team_worker_ids_for_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<String>, DbErr> {
    Ok(team_workers_for_project(db, project)
        .await?
        .into_iter()
        .map(|(_, worker)| worker.worker_id)
        .collect())
}
