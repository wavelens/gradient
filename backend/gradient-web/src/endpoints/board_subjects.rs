/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! What a Job Board row works on: the derivation a build job realises, the
//! repository an eval job evaluates.

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
        anchors: &[DerivationBuildId],
        evaluations: &[EvaluationId],
    ) -> Result<Self, DbErr> {
        Ok(Self {
            derivations: derivation_names(db, anchors).await?,
            repositories: repositories(db, evaluations).await?,
        })
    }

    /// A build job names its derivation, an eval job its repository.
    pub fn subject(
        &self,
        anchor: Option<DerivationBuildId>,
        evaluation: EvaluationId,
    ) -> Option<String> {
        match anchor {
            Some(anchor) => self.derivations.get(&anchor).cloned(),
            None => self.repositories.get(&evaluation).cloned(),
        }
    }
}

async fn derivation_names<C: ConnectionTrait>(
    db: &C,
    anchors: &[DerivationBuildId],
) -> Result<HashMap<DerivationBuildId, String>, DbErr> {
    let by_anchor: Vec<(DerivationId, DerivationBuildId)> =
        gradient_db::fetch_in_chunks(anchors, |chunk| async move {
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

    let derivation_ids: Vec<DerivationId> = by_anchor.iter().map(|(d, _)| *d).collect();
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

    Ok(by_anchor
        .into_iter()
        .filter_map(|(drv, anchor)| names.get(&drv).map(|n| (anchor, n.clone())))
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

/// The evaluation an eval job ran, for the job page.
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
        let (anchor, drv) = (DerivationBuildId::now_v7(), DerivationId::now_v7());
        let evaluation = EvaluationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row([
                ("derivation", drv.into_inner().into()),
                ("id", anchor.into_inner().into()),
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

        let subjects = JobSubjects::load(&db, &[anchor], &[evaluation])
            .await
            .unwrap();

        assert_eq!(
            subjects.subject(Some(anchor), evaluation).as_deref(),
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
