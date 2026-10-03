/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, QueryFilter,
};

pub const BUILD_REQUEST_TASK_NAME: &str = "build-request";

pub fn is_build_request_task(task: &MTask) -> bool {
    task.managed && task.name == BUILD_REQUEST_TASK_NAME
}

pub async fn ensure_build_request_task<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    created_by: UserId,
    keep_evaluations: i32,
) -> Result<MTask, DbErr> {
    if let Some(existing) = find(db, project).await? {
        return Ok(existing);
    }

    ETask::insert(new_task(project, created_by, keep_evaluations).into_active_model())
        .on_conflict_do_nothing_on([CTask::Project, CTask::Name])
        .exec_without_returning(db)
        .await?;

    find(db, project).await?.ok_or_else(|| {
        DbErr::RecordNotFound(format!("{BUILD_REQUEST_TASK_NAME} task of {project}"))
    })
}

fn new_task(project: ProjectId, created_by: UserId, keep_evaluations: i32) -> MTask {
    MTask {
        id: TaskId::now_v7(),
        project,
        name: BUILD_REQUEST_TASK_NAME.to_string(),
        active: true,
        display_name: "Build Requests".to_string(),
        description: "Server-managed task for `gradient build` and SSH build submissions."
            .to_string(),
        repository: BUILD_REQUEST_TASK_NAME.to_string(),
        wildcard: "*".to_string(),
        last_check_at: *NULL_TIME,
        created_by,
        created_at: now(),
        managed: true,
        keep_evaluations,
        concurrency: ConcurrencyPolicy::All,
        sign_cache: true,
        ..Default::default()
    }
}

async fn find<C: ConnectionTrait>(db: &C, project: ProjectId) -> Result<Option<MTask>, DbErr> {
    ETask::find()
        .filter(
            Condition::all()
                .add(CTask::Project.eq(project))
                .add(CTask::Name.eq(BUILD_REQUEST_TASK_NAME)),
        )
        .one(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    #[tokio::test]
    async fn an_existing_task_is_reused() {
        let existing = MTask {
            name: BUILD_REQUEST_TASK_NAME.to_string(),
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![existing.clone()]])
            .into_connection();

        let task = ensure_build_request_task(&db, ProjectId::now_v7(), UserId::now_v7(), 5)
            .await
            .expect("task");
        assert_eq!(task.id, existing.id);
        assert_eq!(db.into_transaction_log().len(), 1);
    }

    #[tokio::test]
    async fn a_missing_task_is_created_as_managed() {
        let project = ProjectId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MTask>::new()])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .append_query_results([vec![MTask {
                project,
                name: BUILD_REQUEST_TASK_NAME.to_string(),
                managed: true,
                ..Default::default()
            }]])
            .into_connection();

        let task = ensure_build_request_task(&db, project, UserId::now_v7(), 5)
            .await
            .expect("task");
        assert!(task.managed);
        let log = format!("{:?}", db.into_transaction_log());
        assert!(log.contains("INSERT INTO"), "{log}");
        assert!(log.contains("build-request"), "{log}");
    }

    #[tokio::test]
    async fn a_concurrent_insert_returns_the_task_that_won() {
        let winner = MTask {
            id: TaskId::now_v7(),
            name: BUILD_REQUEST_TASK_NAME.to_string(),
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MTask>::new()])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([vec![winner.clone()]])
            .into_connection();

        let task = ensure_build_request_task(&db, ProjectId::now_v7(), UserId::now_v7(), 5)
            .await
            .expect("task");
        assert_eq!(task.id, winner.id);
        let log = db.into_transaction_log();
        assert!(
            log.iter().flat_map(|t| t.statements()).any(|s| s
                .sql
                .contains(r#"ON CONFLICT ("project", "name") DO NOTHING"#)),
            "{log:?}"
        );
    }
}
