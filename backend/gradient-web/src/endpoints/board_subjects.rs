/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::input::vec_to_hex;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect};
use serde::Serialize;
use std::collections::HashMap;

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
    let by_shared_build: Vec<(DerivationId, DerivationBuildId)> =
        gradient_db::fetch_in_chunks(shared_builds, |chunk| async move {
            EDerivationBuild::find()
                .select_only()
                .column(CDerivationBuild::Derivation)
                .column(CDerivationBuild::Id)
                .filter(CDerivationBuild::Id.is_in(chunk))
                .into_tuple()
                .all(db)
                .await
        })
        .await?;

    let derivation_ids: Vec<DerivationId> = by_shared_build.iter().map(|(d, _)| *d).collect();
    let names: HashMap<DerivationId, String> =
        gradient_db::fetch_in_chunks(&derivation_ids, |chunk| async move {
            EDerivation::find()
                .select_only()
                .column(CDerivation::Id)
                .column(CDerivation::Name)
                .filter(CDerivation::Id.is_in(chunk))
                .into_tuple::<(DerivationId, String)>()
                .all(db)
                .await
        })
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
    Ok(
        gradient_db::fetch_in_chunks(evaluations, |chunk| async move {
            EEvaluation::find()
                .select_only()
                .column(CEvaluation::Id)
                .column(CEvaluation::Repository)
                .filter(CEvaluation::Id.is_in(chunk))
                .into_tuple::<(EvaluationId, String)>()
                .all(db)
                .await
        })
        .await?
        .into_iter()
        .collect(),
    )
}

#[derive(Serialize)]
pub struct JobEvaluationView {
    pub repository: String,
    pub commit: String,
    pub commit_message: Option<String>,
    pub wildcard: String,
    pub task: Option<String>,
}

pub async fn job_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Option<JobEvaluationView>, DbErr> {
    let Some(ev) = EEvaluation::find_by_id(evaluation).one(db).await? else {
        return Ok(None);
    };

    let commit = ECommit::find_by_id(ev.commit).one(db).await?;
    let task = match ev.task {
        Some(task) => ETask::find_by_id(task).one(db).await?.map(|t| t.name),
        None => None,
    };

    Ok(Some(JobEvaluationView {
        repository: ev.repository,
        commit: commit
            .as_ref()
            .map(|c| vec_to_hex(&c.hash))
            .unwrap_or_default(),
        commit_message: commit.and_then(|c| c.message.lines().next().map(str::to_string)),
        wildcard: ev.wildcard,
        task,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;

    fn row(pairs: [(&str, Value); 2]) -> BTreeMap<&str, Value> {
        BTreeMap::from(pairs)
    }

    #[tokio::test]
    async fn a_build_names_its_derivation_and_an_eval_its_repository() {
        let (shared_build, drv) = (DerivationBuildId::now_v7(), DerivationId::now_v7());
        let evaluation = EvaluationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
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
    async fn nothing_to_name_asks_nothing() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        let subjects = JobSubjects::load(&db, &[], &[]).await.unwrap();

        assert!(subjects.subject(None, EvaluationId::now_v7()).is_none());
        assert!(db.into_transaction_log().is_empty());
    }
}
