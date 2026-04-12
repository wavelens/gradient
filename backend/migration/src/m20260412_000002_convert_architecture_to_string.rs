/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Convert the `architecture` columns in `derivation`, `build_machine_architecture`,
//! and `server_architecture` from Integer (enum discriminant) to Text (free-form
//! Nix system string, e.g. `"x86_64-linux"`).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const ARCH_CASE: &str = "CASE \
    WHEN architecture = 0 THEN 'builtin' \
    WHEN architecture = 1 THEN 'x86_64-linux' \
    WHEN architecture = 2 THEN 'aarch64-linux' \
    WHEN architecture = 3 THEN 'x86_64-darwin' \
    WHEN architecture = 4 THEN 'aarch64-darwin' \
    ELSE 'unknown' \
    END";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        for table in &["derivation", "build_machine_architecture", "server_architecture"] {
            // Add a temporary text column.
            db.execute_unprepared(&format!(
                "ALTER TABLE \"{table}\" ADD COLUMN architecture_text TEXT NOT NULL DEFAULT ''"
            ))
            .await?;

            // Populate it from the integer column using a CASE expression.
            db.execute_unprepared(&format!(
                "UPDATE \"{table}\" SET architecture_text = {ARCH_CASE}"
            ))
            .await?;

            // Drop the old integer column and rename the new one.
            db.execute_unprepared(&format!(
                "ALTER TABLE \"{table}\" DROP COLUMN architecture"
            ))
            .await?;

            db.execute_unprepared(&format!(
                "ALTER TABLE \"{table}\" RENAME COLUMN architecture_text TO architecture"
            ))
            .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        let reverse_case = "CASE \
            WHEN architecture = 'builtin' THEN 0 \
            WHEN architecture = 'x86_64-linux' THEN 1 \
            WHEN architecture = 'aarch64-linux' THEN 2 \
            WHEN architecture = 'x86_64-darwin' THEN 3 \
            WHEN architecture = 'aarch64-darwin' THEN 4 \
            ELSE 0 \
            END";

        for table in &["derivation", "build_machine_architecture", "server_architecture"] {
            db.execute_unprepared(&format!(
                "ALTER TABLE \"{table}\" ADD COLUMN architecture_int SMALLINT NOT NULL DEFAULT 0"
            ))
            .await?;

            db.execute_unprepared(&format!(
                "UPDATE \"{table}\" SET architecture_int = {reverse_case}"
            ))
            .await?;

            db.execute_unprepared(&format!(
                "ALTER TABLE \"{table}\" DROP COLUMN architecture"
            ))
            .await?;

            db.execute_unprepared(&format!(
                "ALTER TABLE \"{table}\" RENAME COLUMN architecture_int TO architecture"
            ))
            .await?;
        }

        Ok(())
    }
}
