/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::exec_job::{self, ExecJobStatus};
use gradient_types::exec::{ExecDefaults, ExecSpec, exec_image_attr};
use gradient_types::{
    CExecJob, DerivationId, EExecJob, EntryPointId, EvaluationId, ExecJobId, MExecJob,
};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, FromQueryResult, IntoActiveModel, Value};
use std::sync::LazyLock;

#[derive(Debug, FromQueryResult)]
struct Candidate {
    entry_point: EntryPointId,
    program_derivation: DerivationId,
    architecture: String,
    exec: serde_json::Value,
    image_derivation: Option<DerivationId>,
}

static CANDIDATES_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT e.id AS entry_point, e.derivation AS program_derivation, d.architecture, e.exec, \
         img.derivation AS image_derivation \
         FROM entry_point e \
         JOIN derivation d ON d.id = e.derivation \
         LEFT JOIN entry_point img ON img.evaluation = e.evaluation \
              AND img.eval = e.eval || '{image_suffix}' \
         WHERE e.evaluation = $1 AND e.exec IS NOT NULL \
         AND NOT EXISTS (SELECT 1 FROM exec_job r WHERE r.entry_point = e.id)",
        image_suffix = exec_image_attr(""),
    )
});

crate::sql_lazy! {
    EXEC_JOB_CANDIDATES = || CANDIDATES_SQL.as_str(),
        params = [EvaluationId];
}

pub async fn for_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<MExecJob>, DbErr> {
    let candidates = Candidate::find_by_statement(
        EXEC_JOB_CANDIDATES.bind([Value::Uuid(Some(evaluation.into_inner()))]),
    )
    .all(db)
    .await?;
    let rows: Vec<exec_job::ActiveModel> = candidates
        .into_iter()
        .map(|candidate| created_job(evaluation, candidate))
        .map(IntoActiveModel::into_active_model)
        .collect();

    EExecJob::insert_many(rows)
        .on_conflict(
            OnConflict::column(CExecJob::EntryPoint)
                .do_nothing()
                .to_owned(),
        )
        .exec_with_returning(db)
        .await
}

fn created_job(evaluation: EvaluationId, candidate: Candidate) -> MExecJob {
    let job = MExecJob {
        id: ExecJobId::now_v7(),
        evaluation,
        entry_point: candidate.entry_point,
        program_derivation: candidate.program_derivation,
        image_derivation: candidate.image_derivation,
        created_at: gradient_types::now(),
        ..Default::default()
    };
    match serde_json::from_value::<ExecSpec>(candidate.exec.clone()) {
        Ok(spec) => MExecJob {
            requirement: serde_json::to_value(
                spec.requirement_on(&candidate.architecture, &ExecDefaults::default()),
            )
            .unwrap_or_default(),
            retries_left: i32::try_from(spec.retries).unwrap_or(i32::MAX),
            status: ExecJobStatus::Created,
            spec: candidate.exec,
            ..job
        },
        Err(error) => {
            tracing::warn!(entry_point = %candidate.entry_point, %error, "exec spec does not decode");
            MExecJob {
                status: ExecJobStatus::Failed,
                message: Some("the exec spec could not be read".to_owned()),
                finished_at: Some(job.created_at),
                spec: candidate.exec,
                ..job
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(exec: serde_json::Value) -> Candidate {
        Candidate {
            entry_point: EntryPointId::now_v7(),
            program_derivation: DerivationId::now_v7(),
            architecture: "x86_64-linux".into(),
            exec,
            image_derivation: None,
        }
    }

    #[test]
    fn a_spec_that_does_not_decode_fails_its_job_instead_of_dropping_it() {
        let job = created_job(
            EvaluationId::now_v7(),
            candidate(serde_json::json!({ "nodes": "two" })),
        );
        assert_eq!(job.status, ExecJobStatus::Failed);
        assert_eq!(
            job.message.as_deref(),
            Some("the exec spec could not be read")
        );
        assert!(job.finished_at.is_some());
    }

    #[test]
    fn a_decoded_spec_waits_for_its_builds_with_its_retries() {
        let job = created_job(
            EvaluationId::now_v7(),
            candidate(
                serde_json::to_value(ExecSpec {
                    retries: 2,
                    ..ExecSpec::default()
                })
                .unwrap(),
            ),
        );
        assert_eq!((job.status, job.retries_left), (ExecJobStatus::Created, 2));
    }
}
