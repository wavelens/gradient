/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Add `evaluation.source_comment` (JSONB) so the comment-driven
//! `/gradient run` / `/gradient approve` pipeline can react to the originating
//! PR comment with +1/-1 when the evaluation reaches a terminal status. Shape:
//! `{ "owner": str, "repo": str, "pr_number": int, "comment_id": int }`.

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let backend = db.get_database_backend();
        db.execute(Statement::from_string(
            backend,
            r#"ALTER TABLE evaluation ADD COLUMN source_comment JSONB"#,
        ))
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let backend = db.get_database_backend();
        db.execute(Statement::from_string(
            backend,
            r#"ALTER TABLE evaluation DROP COLUMN source_comment"#,
        ))
        .await?;
        Ok(())
    }
}
