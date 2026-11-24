/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult};

const STORED_PATHS_OF_TASKS: &str = "SELECT DISTINCT e.task, t.project, cp.id, \
         coalesce(cp.file_size, 0) AS file_size \
     FROM evaluation e \
     JOIN task t                 ON t.id = e.task \
     JOIN build_job bj           ON bj.evaluation = e.id \
     JOIN derivation_output dout ON dout.derivation = bj.derivation \
     JOIN cached_path cp         ON cp.hash = dout.hash";

fn task_stored_bytes_sql() -> String {
    format!(
        "SELECT t.id AS task, t.project, t.name, t.display_name, t.keep_evaluations, \
                (SELECT count(*) FROM evaluation e WHERE e.task = t.id) AS evaluations, \
                (SELECT min(e.created_at) FROM evaluation e WHERE e.task = t.id) \
                    AS oldest_evaluation_at, \
                coalesce(stored.bytes, 0)::bigint AS bytes, \
                coalesce(stored.packages, 0)::bigint AS packages \
         FROM task t \
         LEFT JOIN (SELECT task, sum(file_size) AS bytes, count(*) AS packages \
                    FROM ({STORED_PATHS_OF_TASKS}) paths GROUP BY task) stored \
                ON stored.task = t.id \
         ORDER BY bytes DESC, t.name"
    )
}

fn project_stored_bytes_sql() -> String {
    format!(
        "SELECT project, sum(file_size)::bigint AS bytes, count(*)::bigint AS packages \
         FROM (SELECT DISTINCT project, id, file_size FROM ({STORED_PATHS_OF_TASKS}) paths) owned \
         GROUP BY project \
         ORDER BY bytes DESC"
    )
}

crate::sql_fn! {
    TASK_STORED_BYTES = task_stored_bytes_sql,
        params = [],
        tier = Sweep;

    PROJECT_STORED_BYTES = project_stored_bytes_sql,
        params = [],
        tier = Sweep;
}

crate::sql! {
    CACHE_STORAGE_USAGE = "SELECT c.id AS cache, c.name, c.display_name, c.max_storage_gb, \
            count(cp.id)::bigint AS packages, coalesce(sum(cp.file_size), 0)::bigint AS bytes \
         FROM cache c \
         LEFT JOIN cached_path_signature s ON s.cache = c.id \
         LEFT JOIN cached_path cp          ON cp.id = s.cached_path \
         GROUP BY c.id \
         ORDER BY bytes DESC, c.name",
        params = [],
        tier = Sweep;
}

#[derive(Debug, FromQueryResult)]
pub struct TaskStorageUsageRow {
    pub task: TaskId,
    pub project: ProjectId,
    pub name: String,
    pub display_name: String,
    pub keep_evaluations: i32,
    pub evaluations: i64,
    pub oldest_evaluation_at: Option<NaiveDateTime>,
    pub bytes: i64,
    pub packages: i64,
}

#[derive(Debug, FromQueryResult)]
pub struct ProjectCacheStorageUsageRow {
    pub project: ProjectId,
    pub bytes: i64,
    pub packages: i64,
}

#[derive(Debug, FromQueryResult)]
pub struct CacheStorageUsageRow {
    pub cache: CacheId,
    pub name: String,
    pub display_name: String,
    pub max_storage_gb: i32,
    pub packages: i64,
    pub bytes: i64,
}

pub async fn task_stored_bytes<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<TaskStorageUsageRow>, DbErr> {
    TaskStorageUsageRow::find_by_statement(TASK_STORED_BYTES.stmt())
        .all(db)
        .await
}

pub async fn get_project_storage_usage_rows<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<ProjectCacheStorageUsageRow>, DbErr> {
    ProjectCacheStorageUsageRow::find_by_statement(PROJECT_STORED_BYTES.stmt())
        .all(db)
        .await
}

pub async fn get_cache_storage_usage_rows<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<CacheStorageUsageRow>, DbErr> {
    CacheStorageUsageRow::find_by_statement(CACHE_STORAGE_USAGE.stmt())
        .all(db)
        .await
}

impl From<CacheStorageUsageRow> for gradient_api_types::CacheStorageSummary {
    fn from(row: CacheStorageUsageRow) -> Self {
        Self {
            id: row.cache,
            name: row.name,
            display_name: row.display_name,
            max_storage_gb: row.max_storage_gb,
            packages: row.packages,
            bytes: row.bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::shape::shape;

    const LIB: &str = "b5c0yypb27mfgrhnbhwfq75n9mpmn5ir-lib.drv";
    const LIB_OUT: &str = "1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c";
    const APP: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-app.drv";
    const APP_OUT: &str = "3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e3e";

    #[tokio::test]
    async fn a_path_built_by_two_evaluations_counts_once_for_the_task_and_its_project() {
        let first = EvaluationId::now_v7();
        let second = EvaluationId::now_v7();
        let cache = CacheId::now_v7();
        let db = shape()
            .evaluation(first)
            .evaluation_status(gradient_entity::evaluation::EvaluationStatus::Completed)
            .derivation(LIB)
            .output(LIB_OUT)
            .cached_output(LIB)
            .into_cache(cache)
            .full()
            .job()
            .evaluation(second)
            .same_task_as(first)
            .derivation(LIB)
            .job()
            .derivation(APP)
            .output(APP_OUT)
            .cached_output(APP)
            .job()
            .into_database()
            .await;
        db.execute(&format!(
            "UPDATE cached_path SET file_size = 100 WHERE hash = '{LIB_OUT}'"
        ))
        .await;
        let conn = db.connection();

        let tasks = task_stored_bytes(&conn).await.expect("tasks read");
        assert_eq!(tasks.len(), 1);
        assert_eq!((tasks[0].bytes, tasks[0].packages), (105, 2));
        assert_eq!(tasks[0].evaluations, 2);
        assert!(tasks[0].oldest_evaluation_at.is_some());

        let projects = get_project_storage_usage_rows(&conn)
            .await
            .expect("projects read");
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].project, tasks[0].project);
        assert_eq!((projects[0].bytes, projects[0].packages), (105, 2));

        let caches = get_cache_storage_usage_rows(&conn)
            .await
            .expect("caches read");
        let signed = caches
            .iter()
            .find(|row| row.cache == cache)
            .expect("the cache");
        assert_eq!((signed.packages, signed.bytes), (1, 1 << 30));
        db.finish().await;
    }

    #[tokio::test]
    async fn counts_packages_and_bytes_for_each_variant_once_per_task_and_project_and_zero_bytes_for_tasks_without_evaluations()
     {
        const FOREIGN_PLACEHOLDER: &str = "4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f";
        let (owner, owner_again, foreign) = (
            EvaluationId::now_v7(),
            EvaluationId::now_v7(),
            EvaluationId::now_v7(),
        );
        let second_task_evaluation_id = EvaluationId::now_v7();
        let other_project_task_evaluation_id = EvaluationId::now_v7();
        let evaluation_id_that_will_be_deleted = EvaluationId::now_v7();
        let db = shape()
            .evaluation(owner)
            .evaluation_status(gradient_entity::evaluation::EvaluationStatus::Completed)
            .derivation(LIB)
            .output(LIB_OUT)
            .cached_output(LIB)
            .job()
            .evaluation(owner_again)
            .same_task_as(owner)
            .derivation(LIB)
            .job()
            .evaluation(second_task_evaluation_id)
            .derivation(LIB)
            .job()
            .evaluation(other_project_task_evaluation_id)
            .derivation(LIB)
            .job()
            .evaluation(foreign)
            .cached_path(FOREIGN_PLACEHOLDER, "lib")
            .evaluation(evaluation_id_that_will_be_deleted)
            .into_database()
            .await;
        let project = db.project_of(owner).await;
        let other_project = db.project_of(other_project_task_evaluation_id).await;
        let task_ids = [
            db.task_of(owner).await,
            db.task_of(second_task_evaluation_id).await,
            db.task_of(other_project_task_evaluation_id).await,
            db.task_of(evaluation_id_that_will_be_deleted).await,
        ];
        db.execute(&format!(
            "UPDATE cached_path SET hash = '{LIB_OUT}', nar_hash = 'sha256:foreign', file_size = 100 \
             WHERE hash = '{FOREIGN_PLACEHOLDER}'"
        ))
        .await;
        db.execute(&format!(
            "UPDATE task SET project = '{project}' WHERE id = '{}'",
            task_ids[1]
        ))
        .await;
        db.execute(&format!(
            "DELETE FROM evaluation WHERE id = '{evaluation_id_that_will_be_deleted}'"
        ))
        .await;
        let conn = db.connection();

        let tasks = task_stored_bytes(&conn).await.expect("tasks read");
        let task_usage = task_ids.map(|id| {
            let row = tasks.iter().find(|row| row.task == id).expect("the task");
            (row.bytes, row.packages, row.evaluations)
        });
        assert_eq!(
            task_usage,
            [(105, 2, 2), (105, 2, 1), (105, 2, 1), (0, 0, 0)],
            "Tasks count both stored variants of the output once each. Tasks without evaluation have 0 bytes."
        );

        let projects = get_project_storage_usage_rows(&conn)
            .await
            .expect("projects read");
        let project_usage = [project, other_project].map(|id| {
            let row = projects
                .iter()
                .find(|row| row.project == id)
                .expect("the project");
            (row.bytes, row.packages)
        });
        assert_eq!(
            project_usage,
            [(105, 2), (105, 2)],
            "All projects count each of the two stored variants of an output once, even if two tasks in the same project build that output."
        );
        db.finish().await;
    }
}
