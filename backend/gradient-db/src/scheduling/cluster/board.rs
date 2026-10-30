/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::fetch_in_chunks;
use gradient_entity::cluster_attempt::{
    Column as CClusterAttempt, Entity as EClusterAttempt, Model as MClusterAttempt,
};
use gradient_entity::cluster_job::{Entity as EClusterJob, Model as MClusterJob};
use gradient_entity::cluster_member::{
    Column as CClusterMember, Entity as EClusterMember, Model as MClusterMember,
};
use gradient_entity::dispatched_job::{
    Column as CDispatchedJob, Entity as EDispatchedJob, Model as MDispatchedJob,
};
use gradient_entity::ids::{ClusterAttemptId, ClusterJobId, DispatchedJobId, EvaluationId};
use gradient_types::{
    CEvaluation, CExecJob, CTask, EEvaluation, EExecJob, ETask, ProjectId, TaskId,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ClusterJobView {
    pub cluster: MClusterJob,
    pub project: Option<ProjectId>,
    pub members: Vec<MClusterMember>,
    pub attempts: Vec<MClusterAttempt>,
    pub assignments: Vec<MDispatchedJob>,
}

pub async fn cluster_job_view<C: ConnectionTrait>(
    db: &C,
    id: ClusterJobId,
) -> Result<Option<ClusterJobView>, DbErr> {
    let Some(cluster) = EClusterJob::find_by_id(id).one(db).await? else {
        return Ok(None);
    };
    let members = EClusterMember::find()
        .filter(CClusterMember::ClusterJob.eq(id))
        .order_by_asc(CClusterMember::EvalIndex)
        .order_by_asc(CClusterMember::Id)
        .all(db)
        .await?;
    let attempts = EClusterAttempt::find()
        .filter(CClusterAttempt::ClusterJob.eq(id))
        .order_by_asc(CClusterAttempt::CreatedAt)
        .all(db)
        .await?;
    let attempt_ids: Vec<ClusterAttemptId> = attempts.iter().map(|a| a.id).collect();
    let assignments = assignments_of_attempts(db, &attempt_ids).await?;
    let project = match assignments.first() {
        Some(assignment) => Some(assignment.project),
        None => project_of_members(db, &members).await?,
    };

    Ok(Some(ClusterJobView {
        cluster,
        project,
        members,
        attempts,
        assignments,
    }))
}

async fn assignments_of_attempts<C: ConnectionTrait>(
    db: &C,
    attempts: &[ClusterAttemptId],
) -> Result<Vec<MDispatchedJob>, DbErr> {
    if attempts.is_empty() {
        return Ok(Vec::new());
    }

    EDispatchedJob::find()
        .filter(CDispatchedJob::ClusterAttempt.is_in(attempts.to_vec()))
        .order_by_asc(CDispatchedJob::DispatchedAt)
        .all(db)
        .await
}

async fn project_of_members<C: ConnectionTrait>(
    db: &C,
    members: &[MClusterMember],
) -> Result<Option<ProjectId>, DbErr> {
    let evaluation = match members.iter().find_map(|m| m.evaluation) {
        Some(evaluation) => Some(evaluation),
        None => match members.iter().find_map(|m| m.exec_job) {
            Some(job) => {
                EExecJob::find_by_id(job)
                    .select_only()
                    .column(CExecJob::Evaluation)
                    .into_tuple::<EvaluationId>()
                    .one(db)
                    .await?
            }
            None => None,
        },
    };
    let Some(evaluation) = evaluation else {
        return Ok(None);
    };
    let task = EEvaluation::find_by_id(evaluation)
        .select_only()
        .column(CEvaluation::Task)
        .into_tuple::<Option<TaskId>>()
        .one(db)
        .await?
        .flatten();
    let Some(task) = task else {
        return Ok(None);
    };

    ETask::find_by_id(task)
        .select_only()
        .column(CTask::Project)
        .into_tuple::<ProjectId>()
        .one(db)
        .await
}

pub async fn clusters_of_assignments<C: ConnectionTrait>(
    db: &C,
    assignments: &[DispatchedJobId],
) -> Result<HashMap<DispatchedJobId, ClusterJobId>, DbErr> {
    let attempt_of: Vec<(DispatchedJobId, ClusterAttemptId)> =
        fetch_in_chunks(assignments, |chunk| async move {
            EDispatchedJob::find()
                .select_only()
                .column(CDispatchedJob::Id)
                .column(CDispatchedJob::ClusterAttempt)
                .filter(CDispatchedJob::Id.is_in(chunk))
                .filter(CDispatchedJob::ClusterAttempt.is_not_null())
                .into_tuple::<(DispatchedJobId, ClusterAttemptId)>()
                .all(db)
                .await
        })
        .await?;
    let attempts: Vec<ClusterAttemptId> = attempt_of.iter().map(|(_, a)| *a).collect();
    let cluster_of: HashMap<ClusterAttemptId, ClusterJobId> =
        fetch_in_chunks(&attempts, |chunk| async move {
            EClusterAttempt::find()
                .select_only()
                .column(CClusterAttempt::Id)
                .column(CClusterAttempt::ClusterJob)
                .filter(CClusterAttempt::Id.is_in(chunk))
                .into_tuple::<(ClusterAttemptId, ClusterJobId)>()
                .all(db)
                .await
        })
        .await?
        .into_iter()
        .collect();

    Ok(attempt_of
        .into_iter()
        .filter_map(|(job, attempt)| cluster_of.get(&attempt).map(|c| (job, *c)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::mock_db;
    use sea_orm::Value;
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn an_assignment_outside_a_cluster_maps_to_no_cluster() {
        let (in_cluster, attempt, cluster) = (
            DispatchedJobId::now_v7(),
            ClusterAttemptId::now_v7(),
            ClusterJobId::now_v7(),
        );
        // A tuple query reads a mock row by position, so the keys sort in column order.
        let db = mock_db()
            .append_query_results([vec![BTreeMap::from([
                ("assignment", Value::from(in_cluster.into_inner())),
                ("cluster_attempt", Value::from(attempt.into_inner())),
            ])]])
            .append_query_results([vec![BTreeMap::from([
                ("attempt", Value::from(attempt.into_inner())),
                ("cluster_job", Value::from(cluster.into_inner())),
            ])]])
            .into_connection();

        let clusters = clusters_of_assignments(&db, &[in_cluster, DispatchedJobId::now_v7()])
            .await
            .unwrap();

        assert_eq!(clusters, HashMap::from([(in_cluster, cluster)]));
    }
}
