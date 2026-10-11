/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::metrics_scope::MetricsScope;
use gradient_db::lookup::by_id;
use gradient_types::input::vec_to_hex;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use uuid::Uuid;

pub struct JobSubjects {
    derivations: HashMap<DerivationBuildId, String>,
    repositories: HashMap<EvaluationId, String>,
}

impl JobSubjects {
    pub async fn load<C: ConnectionTrait>(
        db: &C,
        shared_builds: &[DerivationBuildId],
        evaluations: &[EvaluationId],
    ) -> Result<Self, DbErr> {
        Ok(Self {
            derivations: derivation_names(db, shared_builds).await?,
            repositories: repositories(db, evaluations).await?,
        })
    }

    pub fn subject(
        &self,
        shared_build: Option<DerivationBuildId>,
        evaluation: EvaluationId,
    ) -> Option<String> {
        match shared_build {
            Some(shared_build) => self.derivations.get(&shared_build).cloned(),
            None => self.repositories.get(&evaluation).cloned(),
        }
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

#[derive(Serialize, Clone, Default)]
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

#[derive(Serialize)]
pub struct JobEvaluationView {
    pub repository: String,
    pub commit: String,
    pub commit_message: Option<String>,
    pub wildcard: String,
    pub task: Option<String>,
}

impl JobEvaluationView {
    fn new(evaluation: MEvaluation, commit: Option<MCommit>, task: Option<String>) -> Self {
        Self {
            repository: evaluation.repository,
            commit: commit
                .as_ref()
                .map(|c| vec_to_hex(&c.hash))
                .unwrap_or_default(),
            commit_message: commit.and_then(|c| c.message.lines().next().map(str::to_string)),
            wildcard: evaluation.wildcard,
            task,
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
        Some(task) => by_id::<ETask, _>(db, task).await?.map(|t| t.name),
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

        let subjects = JobSubjects::load(&db, &[shared_build], &[evaluation])
            .await
            .unwrap();

        assert_eq!(
            subjects.subject(Some(shared_build), evaluation).as_deref(),
            Some("hello-2.12.1")
        );
        assert_eq!(
            subjects.subject(None, evaluation).as_deref(),
            Some("https://example.org/repo.git")
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

        let subjects = JobSubjects::load(&db, &[], &[]).await.unwrap();
        let names = WorkerNames::load(&db, &MetricsScope::All, [])
            .await
            .unwrap();
        let projects = board_projects(&db, &[]).await.unwrap();

        assert!(subjects.subject(None, EvaluationId::now_v7()).is_none());
        assert!(names.name("w1").is_none() && projects.is_empty());
        assert!(db.into_transaction_log().is_empty());
    }
}
