/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::runner_job::{self, RunnerJobStatus};
use gradient_types::runner::{RunnerDefaults, RunnerSpec, runner_image_attr};
use gradient_types::{
    CRunnerJob, DerivationId, ERunnerJob, EntryPointId, EvaluationId, MRunnerJob, RunnerJobId,
};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, FromQueryResult, IntoActiveModel, Value};
use std::sync::LazyLock;

#[derive(Debug, FromQueryResult)]
struct Candidate {
    entry_point: EntryPointId,
    program_derivation: DerivationId,
    architecture: String,
    runner: serde_json::Value,
    image_derivation: Option<DerivationId>,
}

static CANDIDATES_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT e.id AS entry_point, e.derivation AS program_derivation, d.architecture, e.runner, \
         img.derivation AS image_derivation \
         FROM entry_point e \
         JOIN derivation d ON d.id = e.derivation \
         LEFT JOIN entry_point img ON img.evaluation = e.evaluation \
              AND img.eval = e.eval || '{image_suffix}' \
         WHERE e.evaluation = $1 AND e.runner IS NOT NULL \
         AND NOT EXISTS (SELECT 1 FROM runner_job r WHERE r.entry_point = e.id)",
        image_suffix = runner_image_attr(""),
    )
});

crate::sql_lazy! {
    RUNNER_JOB_CANDIDATES = || CANDIDATES_SQL.as_str(),
        params = [EvaluationId];
}

pub async fn for_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<MRunnerJob>, DbErr> {
    let candidates = Candidate::find_by_statement(
        RUNNER_JOB_CANDIDATES.bind([Value::Uuid(Some(evaluation.into_inner()))]),
    )
    .all(db)
    .await?;
    let rows: Vec<runner_job::ActiveModel> = candidates
        .into_iter()
        .map(|candidate| created_job(evaluation, candidate))
        .map(IntoActiveModel::into_active_model)
        .collect();

    ERunnerJob::insert_many(rows)
        .on_conflict(
            OnConflict::column(CRunnerJob::EntryPoint)
                .do_nothing()
                .to_owned(),
        )
        .exec_with_returning(db)
        .await
}

fn created_job(evaluation: EvaluationId, candidate: Candidate) -> MRunnerJob {
    let job = MRunnerJob {
        id: RunnerJobId::now_v7(),
        evaluation,
        entry_point: candidate.entry_point,
        program_derivation: candidate.program_derivation,
        image_derivation: candidate.image_derivation,
        created_at: gradient_types::now(),
        ..Default::default()
    };
    match serde_json::from_value::<RunnerSpec>(candidate.runner.clone()) {
        Ok(spec) => MRunnerJob {
            requirement: serde_json::to_value(
                spec.requirement_on(&candidate.architecture, &RunnerDefaults::default()),
            )
            .unwrap_or_default(),
            retries_left: i32::try_from(spec.retries).unwrap_or(i32::MAX),
            status: RunnerJobStatus::Created,
            spec: candidate.runner,
            ..job
        },
        Err(error) => {
            tracing::warn!(entry_point = %candidate.entry_point, %error, "runner spec does not decode");
            MRunnerJob {
                status: RunnerJobStatus::Failed,
                message: Some("the runner spec could not be read".to_owned()),
                finished_at: Some(job.created_at),
                spec: candidate.runner,
                ..job
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(runner: serde_json::Value) -> Candidate {
        Candidate {
            entry_point: EntryPointId::now_v7(),
            program_derivation: DerivationId::now_v7(),
            architecture: "x86_64-linux".into(),
            runner,
            image_derivation: None,
        }
    }

    #[test]
    fn a_spec_that_does_not_decode_fails_its_job_instead_of_dropping_it() {
        let job = created_job(
            EvaluationId::now_v7(),
            candidate(serde_json::json!({ "nodes": "two" })),
        );
        assert_eq!(job.status, RunnerJobStatus::Failed);
        assert_eq!(
            job.message.as_deref(),
            Some("the runner spec could not be read")
        );
        assert!(job.finished_at.is_some());
    }

    #[test]
    fn a_decoded_spec_waits_for_its_builds_with_its_retries() {
        let job = created_job(
            EvaluationId::now_v7(),
            candidate(
                serde_json::to_value(RunnerSpec {
                    retries: 2,
                    ..RunnerSpec::default()
                })
                .unwrap(),
            ),
        );
        assert_eq!(
            (job.status, job.retries_left),
            (RunnerJobStatus::Created, 2)
        );
    }
}
