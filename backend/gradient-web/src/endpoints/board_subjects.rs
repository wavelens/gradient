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

#[derive(Default)]
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
    let by_anchor: Vec<(DerivationBuildId, DerivationId)> =
        gradient_db::fetch_in_chunks(anchors, |chunk| async move {
            EDerivationBuild::find()
                .select_only()
                .column(CDerivationBuild::Id)
                .column(CDerivationBuild::Derivation)
                .filter(CDerivationBuild::Id.is_in(chunk))
                .into_tuple()
                .all(db)
                .await
        })
        .await?;

    let derivation_ids: Vec<DerivationId> = by_anchor.iter().map(|(_, d)| *d).collect();
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
        .filter_map(|(anchor, drv)| names.get(&drv).map(|n| (anchor, n.clone())))
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
