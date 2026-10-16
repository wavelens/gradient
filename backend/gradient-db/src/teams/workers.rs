/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter,
};
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

pub async fn teams_granting_workers<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<TeamId>, DbErr> {
    Ok(ETeamProject::find()
        .filter(CTeamProject::Project.eq(project))
        .filter(CTeamProject::IncludesWorkers.eq(true))
        .all(db)
        .await?
        .into_iter()
        .map(|grant| grant.team)
        .collect())
}

pub async fn team_workers_for_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<(String, MTeamWorker)>, DbErr> {
    let teams = teams_granting_workers(db, project).await?;
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

pub async fn worker_of_team<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    worker_id: &str,
) -> Result<Option<MTeamWorker>, DbErr> {
    ETeamWorker::find()
        .filter(CTeamWorker::Team.eq(team))
        .filter(CTeamWorker::WorkerId.eq(worker_id))
        .one(db)
        .await
}

pub async fn workers_of<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
) -> Result<Vec<MTeamWorker>, DbErr> {
    ETeamWorker::find()
        .filter(CTeamWorker::Team.eq(team))
        .all(db)
        .await
}

pub async fn register<C: ConnectionTrait>(
    db: &C,
    worker: MTeamWorker,
) -> Result<MTeamWorker, DbErr> {
    worker.into_active_model().insert(db).await
}

pub async fn update<C: ConnectionTrait>(db: &C, worker: ATeamWorker) -> Result<MTeamWorker, DbErr> {
    worker.update(db).await
}

pub async fn unregister<C: ConnectionTrait>(db: &C, worker: MTeamWorker) -> Result<(), DbErr> {
    worker.into_active_model().delete(db).await?;
    Ok(())
}

pub async fn by_worker_id_or_gradient_ci_team<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
    team: Option<TeamId>,
) -> Result<Option<MTeamWorker>, DbErr> {
    let mut team_workers = Condition::any().add(CTeamWorker::WorkerId.eq(worker_id));
    if let Some(team) = team {
        team_workers = team_workers.add(
            Condition::all()
                .add(CTeamWorker::Team.eq(team))
                .add(CTeamWorker::GradientCi.eq(true)),
        );
    }

    ETeamWorker::find().filter(team_workers).one(db).await
}
