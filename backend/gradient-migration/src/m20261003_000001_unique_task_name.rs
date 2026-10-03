/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

macro_rules! build_request_duplicates {
    () => {
        "(SELECT id, first_value(id) OVER (PARTITION BY project ORDER BY created_at, id) AS survivor \
         FROM task WHERE name = 'build-request') d"
    };
}

const UP: &[&str] = &[
    concat!(
        "UPDATE evaluation t SET task = d.survivor FROM ",
        build_request_duplicates!(),
        " WHERE t.task = d.id AND d.id <> d.survivor"
    ),
    concat!(
        "UPDATE entry_point t SET task = d.survivor FROM ",
        build_request_duplicates!(),
        " WHERE t.task = d.id AND d.id <> d.survivor"
    ),
    concat!(
        "UPDATE dispatched_job t SET task = d.survivor FROM ",
        build_request_duplicates!(),
        " WHERE t.task = d.id AND d.id <> d.survivor"
    ),
    concat!(
        r#"INSERT INTO user_task_star ("user", task, created_at) SELECT s."user", d.survivor, s.created_at FROM user_task_star s JOIN "#,
        build_request_duplicates!(),
        " ON s.task = d.id WHERE d.id <> d.survivor ON CONFLICT DO NOTHING"
    ),
    concat!(
        "DELETE FROM task t USING ",
        build_request_duplicates!(),
        " WHERE t.id = d.id AND d.id <> d.survivor"
    ),
    "UPDATE task t SET name = t.name || '-' || right(t.id::text, 12) \
     FROM (SELECT id, row_number() OVER (PARTITION BY project, name ORDER BY created_at, id) AS n FROM task) d \
     WHERE t.id = d.id AND d.n > 1",
    r#"CREATE UNIQUE INDEX IF NOT EXISTS "uq-task-project-name" ON task (project, name)"#,
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for statement in UP {
            db.execute_unprepared(statement).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(r#"DROP INDEX IF EXISTS "uq-task-project-name""#)
            .await?;

        Ok(())
    }
}
