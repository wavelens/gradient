/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::cluster_job::ClusterJobStatus;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_types::exec::{ExecSpec, exec_refusal};
use gradient_types::{ExecJobId, MExecJob};
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbErr, FromQueryResult, TransactionTrait, Value,
};
use std::sync::LazyLock;

use super::create::{insert_clusters, retries_of};
use super::settle::build_in_state;
use crate::sql::StatusWrite;
use crate::sql::status::{exec, exec_in};

const FINISHED: [ExecJobStatus; 3] = [
    ExecJobStatus::Completed,
    ExecJobStatus::Failed,
    ExecJobStatus::Aborted,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RunRefusal {
    #[error("the exec job has not finished")]
    Open,
    #[error("{0}")]
    Disabled(&'static str),
    #[error("the program of the exec job is no longer built; start a new evaluation")]
    ProgramGone,
    #[error("the exec spec could not be read")]
    UnreadableSpec,
}

#[derive(Debug, Clone, FromQueryResult)]
struct RunState {
    spec: serde_json::Value,
    finished: bool,
    built: bool,
    project_enabled: bool,
    task_enabled: bool,
}

static RUN_STATE_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT r.spec, p.exec_enabled AS project_enabled, t.exec_enabled AS task_enabled, \
         NOT EXISTS (SELECT 1 FROM exec_job o \
              WHERE o.entry_point = r.entry_point AND o.status IN ({open})) \
         AND NOT EXISTS (SELECT 1 FROM cluster_member m \
              JOIN cluster_job c ON c.id = m.cluster_job JOIN exec_job o ON o.id = m.exec_job \
              WHERE o.entry_point = r.entry_point AND c.status = {cluster_running}) AS finished, \
         {program_done} AND (r.image_derivation IS NULL OR {image_done}) AS built \
         FROM exec_job r JOIN evaluation ev ON ev.id = r.evaluation \
         JOIN task t ON t.id = ev.task JOIN project p ON p.id = t.project \
         WHERE r.id = $1",
        open = exec_in(&ExecJobStatus::OPEN),
        cluster_running = i16::from(ClusterJobStatus::Running),
        program_done = build_in_state("r.program_derivation", &BuildStatus::TERMINAL_SUCCESS),
        image_done = build_in_state("r.image_derivation", &BuildStatus::TERMINAL_SUCCESS),
    )
});

static REQUEUE_FINISHED_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE exec_job SET status = {created}, retries_left = $2, message = NULL, \
         waiting_reason = NULL, started_at = NULL, finished_at = NULL \
         WHERE entry_point = (SELECT entry_point FROM exec_job WHERE id = $1) \
         AND status IN ({finished}) RETURNING *",
        created = exec(ExecJobStatus::Created),
        finished = exec_in(&FINISHED),
    )
});

crate::sql_lazy! {
    EXEC_JOB_RUN_STATE = || RUN_STATE_SQL.as_str(),
        params = [NewUuid];
    REQUEUE_FINISHED_EXEC_JOB = || REQUEUE_FINISHED_SQL.as_str(),
        params = [NewUuid, Int(3)],
        status_write = StatusWrite::exec_job(ExecJobStatus::Created);
}

crate::sql! {
    DELETE_CLUSTER_JOB_OF_EXEC_JOB = "DELETE FROM cluster_job c WHERE c.id IN \
         (SELECT m.cluster_job FROM cluster_member m JOIN exec_job r ON r.id = m.exec_job \
          WHERE r.entry_point = (SELECT entry_point FROM exec_job WHERE id = $1))",
        params = [NewUuid];
}

pub async fn requeue<C>(
    db: &C,
    job: ExecJobId,
    instance_enabled: bool,
) -> Result<Result<Vec<MExecJob>, RunRefusal>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let id = Value::Uuid(Some(job.into_inner()));
    let Some(state) = RunState::find_by_statement(EXEC_JOB_RUN_STATE.bind([id.clone()]))
        .one(db)
        .await?
    else {
        return Ok(Ok(Vec::new()));
    };
    let retries = match fresh_retries(&state, instance_enabled) {
        Ok(retries) => retries,
        Err(refusal) => return Ok(Err(refusal)),
    };

    let txn = db.begin().await?;
    let rows = MExecJob::find_by_statement(
        REQUEUE_FINISHED_EXEC_JOB.bind([id.clone(), Value::Int(Some(retries))]),
    )
    .all(&txn)
    .await?;
    txn.execute_raw(DELETE_CLUSTER_JOB_OF_EXEC_JOB.bind([id]))
        .await?;
    insert_clusters(&txn, &rows).await?;
    txn.commit().await?;
    Ok(Ok(rows))
}

fn fresh_retries(state: &RunState, instance_enabled: bool) -> Result<i32, RunRefusal> {
    if !state.finished {
        return Err(RunRefusal::Open);
    }
    if let Some(message) = exec_refusal(instance_enabled, state.project_enabled, state.task_enabled)
    {
        return Err(RunRefusal::Disabled(message));
    }
    if !state.built {
        return Err(RunRefusal::ProgramGone);
    }
    serde_json::from_value::<ExecSpec>(state.spec.clone())
        .map(|spec| retries_of(&spec))
        .map_err(|_| RunRefusal::UnreadableSpec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished_job() -> RunState {
        RunState {
            spec: serde_json::to_value(ExecSpec {
                retries: 2,
                ..ExecSpec::default()
            })
            .unwrap(),
            finished: true,
            built: true,
            project_enabled: true,
            task_enabled: true,
        }
    }

    #[test]
    fn a_finished_job_runs_again_with_the_retries_of_its_spec() {
        assert_eq!(fresh_retries(&finished_job(), true), Ok(2));
    }

    #[test]
    fn an_open_job_is_refused_before_its_switches() {
        let state = RunState {
            finished: false,
            task_enabled: false,
            ..finished_job()
        };
        assert_eq!(fresh_retries(&state, true), Err(RunRefusal::Open));
    }

    #[test]
    fn a_switch_still_off_is_named_in_the_refusal() {
        let state = RunState {
            task_enabled: false,
            ..finished_job()
        };
        assert_eq!(
            fresh_retries(&state, true).map_err(|refusal| refusal.to_string()),
            Err("exec jobs are disabled for this task".to_owned())
        );
        assert_eq!(
            fresh_retries(&finished_job(), false).map_err(|refusal| refusal.to_string()),
            Err("exec jobs are disabled on this instance".to_owned())
        );
    }

    #[test]
    fn a_program_no_longer_built_or_an_unreadable_spec_is_refused() {
        let unbuilt = RunState {
            built: false,
            ..finished_job()
        };
        assert_eq!(fresh_retries(&unbuilt, true), Err(RunRefusal::ProgramGone));
        let unreadable = RunState {
            spec: serde_json::json!({ "nodes": "two" }),
            ..finished_job()
        };
        assert_eq!(
            fresh_retries(&unreadable, true),
            Err(RunRefusal::UnreadableSpec)
        );
    }
}
