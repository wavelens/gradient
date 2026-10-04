/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::sea_query::Query;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, FromQueryResult, JoinType, QueryFilter,
    QueryOrder, QuerySelect, RelationTrait,
};
use serde::Serialize;

#[derive(Debug, FromQueryResult, Serialize)]
pub struct TeamEvaluation {
    pub id: EvaluationId,
    pub project: String,
    pub task: String,
    pub status: gradient_entity::evaluation::EvaluationStatus,
    pub created_at: chrono::NaiveDateTime,
}

pub async fn recent_evaluations<C: ConnectionTrait>(
    db: &C,
    team: TeamId,
    limit: u64,
) -> Result<Vec<TeamEvaluation>, DbErr> {
    let shared_projects = Query::select()
        .column(CTeamProject::Project)
        .from(gradient_entity::team_project::Entity)
        .and_where(CTeamProject::Team.eq(team))
        .and_where(CTeamProject::IncludesUsers.eq(true))
        .to_owned();

    EEvaluation::find()
        .select_only()
        .column(CEvaluation::Id)
        .column_as(CProject::Name, "project")
        .column_as(CTask::Name, "task")
        .column(CEvaluation::Status)
        .column(CEvaluation::CreatedAt)
        .join(
            JoinType::InnerJoin,
            gradient_entity::evaluation::Relation::Task.def(),
        )
        .join(
            JoinType::InnerJoin,
            gradient_entity::task::Relation::Project.def(),
        )
        .filter(CTask::Project.in_subquery(shared_projects))
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(limit)
        .into_model::<TeamEvaluation>()
        .all(db)
        .await
}
