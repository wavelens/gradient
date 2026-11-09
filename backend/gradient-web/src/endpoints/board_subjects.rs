/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::metrics_scope::MetricsScope;
use gradient_db::exec_jobs::labels::ExecJobLabel;
use gradient_db::lookup::{by_id, by_ids};
use gradient_db::scheduling::assignment_record::{exec_job_of_key, shared_build_of_key};
use gradient_entity::dispatched_job::DispatchedJobKind;
use gradient_types::input::vec_to_hex;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JobSubject {
    Build(DerivationBuildId),
    Evaluation(EvaluationId),
    Exec(ExecJobId),
}

impl JobSubject {
    pub fn of_key(
        kind: DispatchedJobKind,
        job_key: Option<&str>,
        evaluation: EvaluationId,
    ) -> Option<Self> {
        match kind {
            DispatchedJobKind::Eval => Some(Self::Evaluation(evaluation)),
            DispatchedJobKind::Build => job_key.and_then(shared_build_of_key).map(Self::Build),
            DispatchedJobKind::Exec => job_key.and_then(exec_job_of_key).map(Self::Exec),
        }
    }

    pub fn shared_build(self) -> Option<DerivationBuildId> {
        match self {
            Self::Build(shared_build) => Some(shared_build),
            Self::Evaluation(_) | Self::Exec(_) => None,
        }
    }

    fn exec_job(self) -> Option<ExecJobId> {
        match self {
            Self::Exec(job) => Some(job),
            Self::Build(_) | Self::Evaluation(_) => None,
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq, utoipa::ToSchema)]
pub struct BoardExecJob {
    pub id: Uuid,
    pub project: String,
    pub entry_point: String,
    pub task: String,
    pub node_index: u32,
    pub nodes: u32,
}

impl From<ExecJobLabel> for BoardExecJob {
    fn from(label: ExecJobLabel) -> Self {
        Self {
            id: label.id.into(),
            project: label.project,
            entry_point: label.entry_point,
            task: label.task,
            node_index: label.node_index,
            nodes: label.nodes,
        }
    }
}

pub struct JobSubjects {
    derivations: HashMap<DerivationBuildId, String>,
    repositories: HashMap<EvaluationId, String>,
    exec_jobs: HashMap<ExecJobId, BoardExecJob>,
    exec_waiting_reasons: HashMap<ExecJobId, WaitingReason>,
}

impl JobSubjects {
    pub async fn load<C: ConnectionTrait>(
        db: &C,
        subjects: impl IntoIterator<Item = JobSubject>,
    ) -> Result<Self, DbErr> {
        let (mut shared_builds, mut evaluations, mut exec_jobs) =
            (Vec::new(), Vec::new(), Vec::new());
        for subject in subjects.into_iter().collect::<BTreeSet<_>>() {
            match subject {
                JobSubject::Build(shared_build) => shared_builds.push(shared_build),
                JobSubject::Evaluation(evaluation) => evaluations.push(evaluation),
                JobSubject::Exec(job) => exec_jobs.push(job),
            }
        }

        let labels = gradient_db::exec_jobs::labels::labels(db, &exec_jobs).await?;
        let exec_waiting_reasons = labels
            .iter()
            .filter_map(|label| Some((label.id, label.waiting_reason.clone()?)))
            .collect();

        Ok(Self {
            derivations: derivation_names(db, &shared_builds).await?,
            repositories: repositories(db, &evaluations).await?,
            exec_jobs: labels
                .into_iter()
                .map(|label| (label.id, BoardExecJob::from(label)))
                .collect(),
            exec_waiting_reasons,
        })
    }

    pub fn waiting_reason_for_pending_job(
        &self,
        subject: JobSubject,
        evaluation: Option<WaitingReason>,
    ) -> Option<WaitingReason> {
        match subject {
            JobSubject::Exec(job) => self.exec_waiting_reasons.get(&job).cloned(),
            JobSubject::Build(_) | JobSubject::Evaluation(_) => evaluation,
        }
    }

    pub fn subject(&self, subject: Option<JobSubject>) -> Option<String> {
        match subject? {
            JobSubject::Build(shared_build) => self.derivations.get(&shared_build).cloned(),
            JobSubject::Evaluation(evaluation) => self.repositories.get(&evaluation).cloned(),
            JobSubject::Exec(job) => self.exec_jobs.get(&job).map(|j| j.entry_point.clone()),
        }
    }

    pub fn exec_job(&self, subject: Option<JobSubject>) -> Option<BoardExecJob> {
        self.exec_jobs.get(&subject?.exec_job()?).cloned()
    }
}

async fn derivation_names<C: ConnectionTrait>(
    db: &C,
    shared_builds: &[DerivationBuildId],
) -> Result<HashMap<DerivationBuildId, String>, DbErr> {
    let by_shared_build = gradient_db::derivation_builds::derivations_of(db, shared_builds).await?;

    let derivation_ids: Vec<DerivationId> = by_shared_build.iter().map(|(d, _)| *d).collect();
    let names: HashMap<DerivationId, String> = gradient_db::derivations::names(db, &derivation_ids)
        .await?
        .into_iter()
        .collect();

    Ok(by_shared_build
        .into_iter()
        .filter_map(|(drv, shared_build)| names.get(&drv).map(|n| (shared_build, n.clone())))
        .collect())
}

async fn repositories<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
) -> Result<HashMap<EvaluationId, String>, DbErr> {
    Ok(gradient_db::evaluations::repositories(db, evaluations)
        .await?
        .into_iter()
        .collect())
}

/// A worker's display name comes only from registrations and team workers in projects the caller can see.
pub struct WorkerNames(HashMap<String, String>);

impl WorkerNames {
    pub async fn load<C: ConnectionTrait>(
        db: &C,
        scope: &MetricsScope,
        workers: impl IntoIterator<Item = String>,
    ) -> Result<Self, DbErr> {
        let workers: Vec<String> = workers
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let visible = scope.project_ids();
        let registrations =
            gradient_db::worker_names::of_registrations(db, &workers, visible.as_deref()).await?;
        let team_workers =
            gradient_db::worker_names::of_team_workers(db, &workers, visible.as_deref()).await?;

        let mut names = HashMap::new();
        for row in registrations.into_iter().chain(team_workers) {
            names.entry(row.worker_id).or_insert(row.display_name);
        }

        Ok(Self(names))
    }

    pub fn name(&self, worker: &str) -> Option<String> {
        self.0.get(worker).cloned()
    }
}

#[derive(Serialize, Clone, Default, utoipa::ToSchema)]
pub struct BoardProject {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
}

pub async fn board_projects<C: ConnectionTrait>(
    db: &C,
    projects: &[Uuid],
) -> Result<HashMap<Uuid, BoardProject>, DbErr> {
    let rows = gradient_db::projects::names(db, projects).await?;

    Ok(rows
        .into_iter()
        .map(|(id, name, display_name)| {
            (
                id,
                BoardProject {
                    id,
                    name,
                    display_name,
                },
            )
        })
        .collect())
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PendingEvaluation {
    pub task: Option<String>,
    pub task_display_name: Option<String>,
    pub waiting_reason: Option<WaitingReason>,
}

pub async fn pending_evaluations<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
) -> Result<HashMap<EvaluationId, PendingEvaluation>, DbErr> {
    if evaluations.is_empty() {
        return Ok(HashMap::new());
    }
    let evaluations = by_ids::<EEvaluation, _>(db, evaluations).await?;
    let task_ids: Vec<TaskId> = evaluations.values().filter_map(|e| e.task).collect();
    let tasks = by_ids::<ETask, _>(db, &task_ids).await?;

    Ok(evaluations
        .into_iter()
        .map(|(id, evaluation)| {
            let task = evaluation.task.and_then(|task| tasks.get(&task));
            let waiting_reason = WaitingReason::from_status_and_json_if_waiting(
                evaluation.status,
                evaluation.waiting_reason.as_ref(),
            );
            (
                id,
                PendingEvaluation {
                    task: task.map(|t| t.name.clone()),
                    task_display_name: task.map(|t| t.display_name.clone()),
                    waiting_reason,
                },
            )
        })
        .collect())
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct JobEvaluationView {
    pub repository: String,
    pub commit: String,
    pub commit_message: Option<String>,
    pub wildcard: String,
    pub task: Option<String>,
    pub task_display_name: Option<String>,
}

impl JobEvaluationView {
    fn new(evaluation: MEvaluation, commit: Option<MCommit>, task: Option<MTask>) -> Self {
        Self {
            repository: evaluation.repository,
            commit: commit
                .as_ref()
                .map(|c| vec_to_hex(&c.hash))
                .unwrap_or_default(),
            commit_message: commit.and_then(|c| c.message.lines().next().map(str::to_string)),
            wildcard: evaluation.wildcard,
            task_display_name: task.as_ref().map(|t| t.display_name.clone()),
            task: task.map(|t| t.name),
        }
    }
}

pub async fn job_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Option<JobEvaluationView>, DbErr> {
    let Some(ev) = by_id::<EEvaluation, _>(db, evaluation).await? else {
        return Ok(None);
    };

    let commit = by_id::<ECommit, _>(db, ev.commit).await?;
    let task = match ev.task {
        Some(task) => by_id::<ETask, _>(db, task).await?,
        None => None,
    };

    Ok(Some(JobEvaluationView::new(ev, commit, task)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{empty_db, mock_db};
    use sea_orm::Value;
    use std::collections::BTreeMap;

    fn row(pairs: [(&str, Value); 2]) -> BTreeMap<&str, Value> {
        BTreeMap::from(pairs)
    }

    #[tokio::test]
    async fn a_build_names_its_derivation_and_an_eval_its_repository() {
        let (shared_build, drv) = (DerivationBuildId::now_v7(), DerivationId::now_v7());
        let evaluation = EvaluationId::now_v7();
        let db = mock_db()
            .append_query_results([vec![row([
                ("derivation", drv.into_inner().into()),
                ("id", shared_build.into_inner().into()),
            ])]])
            .append_query_results([vec![row([
                ("id", drv.into_inner().into()),
                ("name", "hello-2.12.1".into()),
            ])]])
            .append_query_results([vec![row([
                ("id", evaluation.into_inner().into()),
                ("repository", "https://example.org/repo.git".into()),
            ])]])
            .into_connection();

        let subjects = JobSubjects::load(
            &db,
            [
                JobSubject::Build(shared_build),
                JobSubject::Evaluation(evaluation),
            ],
        )
        .await
        .unwrap();

        assert_eq!(
            subjects
                .subject(Some(JobSubject::Build(shared_build)))
                .as_deref(),
            Some("hello-2.12.1")
        );
        assert_eq!(
            subjects
                .subject(Some(JobSubject::Evaluation(evaluation)))
                .as_deref(),
            Some("https://example.org/repo.git")
        );
    }

    #[tokio::test]
    async fn an_exec_node_is_named_by_its_entry_point_task_and_node() {
        let (entry_point, task) = (EntryPointId::now_v7(), TaskId::now_v7());
        let project = ProjectId::now_v7();
        let node = MExecJob {
            id: ExecJobId::now_v7(),
            entry_point,
            node_index: 1,
            spec: serde_json::to_value(gradient_types::exec::ExecSpec {
                nodes: 2,
                ..Default::default()
            })
            .unwrap(),
            status: gradient_entity::exec_job::ExecJobStatus::Created,
            waits_for_manual_start: true,
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![node.clone()]])
            .append_query_results([vec![BTreeMap::from([
                ("eval", Value::from("apps.x86_64-linux.train")),
                ("id", entry_point.into_inner().into()),
                ("task", task.into_inner().into()),
            ])]])
            .append_query_results([vec![BTreeMap::from([
                ("id", Value::from(task.into_inner())),
                ("name", "nightly".into()),
                ("project", project.into_inner().into()),
            ])]])
            .append_query_results([vec![row([
                ("id", project.into_inner().into()),
                ("name", "infra".into()),
            ])]])
            .into_connection();
        let subject = JobSubject::of_key(
            DispatchedJobKind::Exec,
            Some(&format!("exec:{}", node.id)),
            EvaluationId::now_v7(),
        );

        let subjects = JobSubjects::load(&db, subject).await.unwrap();

        assert_eq!(
            subjects.subject(subject).as_deref(),
            Some("apps.x86_64-linux.train")
        );
        assert_eq!(
            subjects.exec_job(subject),
            Some(BoardExecJob {
                id: node.id.into(),
                project: "infra".into(),
                entry_point: "apps.x86_64-linux.train".into(),
                task: "nightly".into(),
                node_index: 1,
                nodes: 2,
            })
        );
        assert_eq!(
            subjects.waiting_reason_for_pending_job(subject.unwrap(), Some(WaitingReason::NoCache)),
            Some(WaitingReason::ManualStart)
        );
    }

    #[tokio::test]
    async fn a_pending_evaluation_names_its_task_and_only_a_current_waiting_reason() {
        use gradient_entity::evaluation::EvaluationStatus;
        let task = MTask {
            id: TaskId::now_v7(),
            name: "nightly".into(),
            display_name: "Nightly".into(),
            ..Default::default()
        };
        let waiting = MEvaluation {
            id: EvaluationId::now_v7(),
            task: Some(task.id),
            status: EvaluationStatus::Waiting,
            waiting_reason: Some(WaitingReason::NoCache.to_json()),
            ..Default::default()
        };
        let queued = MEvaluation {
            id: EvaluationId::now_v7(),
            status: EvaluationStatus::Queued,
            waiting_reason: Some(WaitingReason::NoCache.to_json()),
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![waiting.clone(), queued.clone()]])
            .append_query_results([vec![task]])
            .into_connection();

        let pending = pending_evaluations(&db, &[waiting.id, queued.id])
            .await
            .unwrap();

        assert_eq!(
            pending[&waiting.id],
            PendingEvaluation {
                task: Some("nightly".into()),
                task_display_name: Some("Nightly".into()),
                waiting_reason: Some(WaitingReason::NoCache),
            }
        );
        assert_eq!(pending[&queued.id], PendingEvaluation::default());
        assert!(
            pending_evaluations(&empty_db(), &[])
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_job_key_picks_the_subject_of_its_kind() {
        let evaluation = EvaluationId::now_v7();
        let shared_build = DerivationBuildId::now_v7();

        assert_eq!(
            JobSubject::of_key(
                DispatchedJobKind::Build,
                Some(&format!("build:{shared_build}")),
                evaluation
            ),
            Some(JobSubject::Build(shared_build))
        );
        assert_eq!(
            JobSubject::of_key(DispatchedJobKind::Eval, Some("eval:x"), evaluation),
            Some(JobSubject::Evaluation(evaluation))
        );
        assert_eq!(
            JobSubject::of_key(DispatchedJobKind::Exec, None, evaluation),
            None
        );
    }

    #[tokio::test]
    async fn a_worker_takes_the_name_of_its_first_ranked_registration_then_its_team_worker() {
        let db = mock_db()
            .append_query_results([vec![
                row([
                    ("worker_id", "w1".into()),
                    ("display_name", "builder".into()),
                ]),
                row([
                    ("worker_id", "w1".into()),
                    ("display_name", "retired".into()),
                ]),
            ]])
            .append_query_results([vec![row([
                ("worker_id", "w2".into()),
                ("display_name", "team builder".into()),
            ])]])
            .into_connection();

        let names = WorkerNames::load(
            &db,
            &MetricsScope::All,
            ["w1".to_string(), "w2".to_string(), "w3".to_string()],
        )
        .await
        .unwrap();

        assert_eq!(names.name("w1").as_deref(), Some("builder"));
        assert_eq!(names.name("w2").as_deref(), Some("team builder"));
        assert_eq!(names.name("w3"), None);
    }

    #[tokio::test]
    async fn nothing_to_name_asks_nothing() {
        let db = empty_db();

        let subjects = JobSubjects::load(&db, []).await.unwrap();
        let names = WorkerNames::load(&db, &MetricsScope::All, [])
            .await
            .unwrap();
        let projects = board_projects(&db, &[]).await.unwrap();

        assert!(
            subjects
                .subject(Some(JobSubject::Evaluation(EvaluationId::now_v7())))
                .is_none()
        );
        assert!(names.name("w1").is_none() && projects.is_empty());
        assert!(db.into_transaction_log().is_empty());
    }
}
