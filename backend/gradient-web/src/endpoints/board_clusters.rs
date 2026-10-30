/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::board_subjects::{BoardExecJob, JobSubject, JobSubjects, WorkerNames, board_projects};
use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::metrics_scope::MetricsScope;
use crate::resource::Resource;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::scheduling::assignment_record::{exec_job_of_key, shared_build_of_key};
use gradient_db::scheduling::cluster::{ClusterJobView, cluster_job_view};
use gradient_entity::cluster_job::ClusterJobStatus;
use gradient_entity::cluster_member::Model as MClusterMember;
use gradient_entity::dispatched_job::{self, DispatchedJobKind, DispatchedJobOutcome};
use gradient_scheduler::jobs::parse_eval_job_key;
use gradient_types::ids::{ClusterJobId, ClusterMemberId};
use gradient_types::*;
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, utoipa::ToSchema)]
pub struct ClusterMemberView {
    pub id: Uuid,
    pub role: String,
    pub primary: bool,
    pub eval_index: Option<i16>,
    pub exec_job: Option<BoardExecJob>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ClusterSubJobView {
    pub id: Uuid,
    pub member: Option<Uuid>,
    pub attempt: u32,
    pub worker_id: String,
    pub worker_name: Option<String>,
    pub dispatched_at: String,
    pub finished_at: Option<String>,
    pub outcome: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ClusterJobDetail {
    pub id: Uuid,
    pub kind: i16,
    pub status: String,
    pub same_zone: bool,
    pub attempts: i32,
    pub retry_budget: i32,
    pub created_at: String,
    pub updated_at: String,
    pub project: Uuid,
    pub project_name: String,
    pub project_display_name: String,
    pub evaluation_id: Option<Uuid>,
    pub subject: Option<String>,
    pub members: Vec<ClusterMemberView>,
    pub sub_jobs: Vec<ClusterSubJobView>,
}

fn status_name(status: ClusterJobStatus) -> &'static str {
    match status {
        ClusterJobStatus::Queued => "queued",
        ClusterJobStatus::Running => "running",
        ClusterJobStatus::Completed => "completed",
        ClusterJobStatus::Failed => "failed",
        ClusterJobStatus::Aborted => "aborted",
    }
}

fn outcome_name(outcome: DispatchedJobOutcome) -> &'static str {
    match outcome {
        DispatchedJobOutcome::Completed => "completed",
        DispatchedJobOutcome::Failed => "failed",
        DispatchedJobOutcome::Abandoned => "abandoned",
    }
}

fn kind_of(members: &[MClusterMember]) -> DispatchedJobKind {
    match members.first() {
        Some(m) if m.exec_job.is_some() => DispatchedJobKind::Exec,
        Some(m) if m.derivation_build.is_some() => DispatchedJobKind::Build,
        _ => DispatchedJobKind::Eval,
    }
}

fn member_of(job: &dispatched_job::Model, members: &[MClusterMember]) -> Option<ClusterMemberId> {
    let key = job.job_id.as_deref()?;
    let matches = |m: &&MClusterMember| match job.kind {
        DispatchedJobKind::Exec => m.exec_job.is_some() && m.exec_job == exec_job_of_key(key),
        DispatchedJobKind::Build => {
            m.derivation_build.is_some() && m.derivation_build == shared_build_of_key(key)
        }
        DispatchedJobKind::Eval => parse_eval_job_key(key).is_some_and(|(evaluation, index)| {
            m.evaluation == Some(evaluation)
                && i32::from(m.eval_index.unwrap_or(0)) == i32::from(index)
        }),
    };
    members.iter().find(matches).map(|m| m.id)
}

fn cluster_subject(view: &ClusterJobView) -> Option<JobSubject> {
    let member = view.members.first()?;
    match (member.exec_job, member.derivation_build, member.evaluation) {
        (Some(job), _, _) => Some(JobSubject::Exec(job)),
        (None, Some(build), _) => Some(JobSubject::Build(build)),
        (None, None, evaluation) => evaluation.map(JobSubject::Evaluation),
    }
}

#[utoipa::path(
    get,
    path = "/board/clusters/{id}",
    operation_id = "getBoardClusterJob",
    tag = "board",
    summary = "Cluster job detail",
    description = "A cluster job with its members and every sub job assigned to a worker over all attempts. Each sub job names its member and opens with `getBoardJob`.",
    params(("id" = Uuid, Path)),
    responses((status = OK, description = "Cluster job detail", body = ClusterJobDetail), WebError)
)]
pub async fn get_cluster_job(
    State(state): State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<ClusterJobDetail>>> {
    let scope = MetricsScope::resolve(&state.web_db, &maybe_user).await?;
    let view = cluster_job_view(&state.web_db, ClusterJobId::from(id))
        .await?
        .ok_or_else(|| WebError::not_found(Resource::Job))?;
    let project_id = view
        .project
        .map(Uuid::from)
        .filter(|project| scope.allows(project))
        .ok_or_else(|| WebError::not_found(Resource::Job))?;

    let project = board_projects(&state.web_db, &[project_id])
        .await?
        .remove(&project_id)
        .unwrap_or_default();
    let member_exec_jobs = view
        .members
        .iter()
        .filter_map(|m| m.exec_job.map(JobSubject::Exec));
    let subject = cluster_subject(&view);
    let subjects = JobSubjects::load(&state.web_db, member_exec_jobs.chain(subject)).await?;
    let worker_names = WorkerNames::load(
        &state.web_db,
        &scope,
        view.assignments.iter().map(|j| j.worker_id.clone()),
    )
    .await?;

    Ok(ok_json(detail(
        view,
        project_id,
        project,
        subject,
        &subjects,
        &worker_names,
    )))
}

fn detail(
    view: ClusterJobView,
    project_id: Uuid,
    project: super::board_subjects::BoardProject,
    subject: Option<JobSubject>,
    subjects: &JobSubjects,
    worker_names: &WorkerNames,
) -> ClusterJobDetail {
    let attempt_number = |attempt| {
        view.attempts
            .iter()
            .position(|a| Some(a.id) == attempt)
            .map_or(0, |i| u32::try_from(i + 1).unwrap_or(u32::MAX))
    };
    let sub_jobs = view
        .assignments
        .iter()
        .map(|j| ClusterSubJobView {
            id: j.id.into(),
            member: member_of(j, &view.members).map(Into::into),
            attempt: attempt_number(j.cluster_attempt),
            worker_name: worker_names.name(&j.worker_id),
            worker_id: j.worker_id.clone(),
            dispatched_at: j.dispatched_at.and_utc().to_rfc3339(),
            finished_at: j.finished_at.map(|t| t.and_utc().to_rfc3339()),
            outcome: j.outcome.map(|o| outcome_name(o).to_owned()),
        })
        .collect();
    let evaluation_id = view
        .members
        .iter()
        .find_map(|m| m.evaluation)
        .or_else(|| view.assignments.first().map(|j| j.evaluation_id));

    ClusterJobDetail {
        id: view.cluster.id.into(),
        kind: i16::from(kind_of(&view.members)),
        status: status_name(view.cluster.status).to_owned(),
        same_zone: view.cluster.same_zone,
        attempts: view.cluster.attempts,
        retry_budget: view.cluster.retry_budget,
        created_at: view.cluster.created_at.and_utc().to_rfc3339(),
        updated_at: view.cluster.updated_at.and_utc().to_rfc3339(),
        project: project_id,
        project_name: project.name,
        project_display_name: project.display_name,
        evaluation_id: evaluation_id.map(Into::into),
        subject: subjects.subject(subject),
        members: view
            .members
            .iter()
            .map(|m| ClusterMemberView {
                id: m.id.into(),
                role: m.role.clone(),
                primary: m.primary,
                eval_index: m.eval_index,
                exec_job: subjects.exec_job(m.exec_job.map(JobSubject::Exec)),
            })
            .collect(),
        sub_jobs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_scheduler::jobs::eval_job_key;

    fn split_member(evaluation: EvaluationId, index: i16) -> MClusterMember {
        MClusterMember {
            id: ClusterMemberId::now_v7(),
            evaluation: Some(evaluation),
            eval_index: Some(index),
            ..Default::default()
        }
    }

    fn assignment(kind: DispatchedJobKind, key: String) -> dispatched_job::Model {
        dispatched_job::Model {
            kind,
            job_id: Some(key),
            ..Default::default()
        }
    }

    #[test]
    fn a_split_evaluation_sub_job_belongs_to_the_member_of_its_index() {
        let evaluation = EvaluationId::now_v7();
        let members = [split_member(evaluation, 0), split_member(evaluation, 1)];

        let first = assignment(DispatchedJobKind::Eval, eval_job_key(evaluation, 0));
        let second = assignment(DispatchedJobKind::Eval, eval_job_key(evaluation, 1));

        assert_eq!(member_of(&first, &members), Some(members[0].id));
        assert_eq!(member_of(&second, &members), Some(members[1].id));
    }

    #[test]
    fn an_exec_sub_job_belongs_to_the_member_of_its_node() {
        let (node0, node1) = (ExecJobId::now_v7(), ExecJobId::now_v7());
        let member = |job| MClusterMember {
            id: ClusterMemberId::now_v7(),
            exec_job: Some(job),
            ..Default::default()
        };
        let members = [member(node0), member(node1)];
        let key = format!(
            "{}{node1}",
            gradient_db::scheduling::assignment_record::EXEC_KEY_PREFIX
        );

        let job = assignment(DispatchedJobKind::Exec, key);

        assert_eq!(member_of(&job, &members), Some(members[1].id));
    }
}
