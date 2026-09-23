/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `prioritized` on `evaluation` and `derivation_build` (#530). The flag falls
//! back to false in the database the moment its row fails for good or is aborted, so every
//! status writer clears it without knowing it exists.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "ALTER TABLE evaluation ADD COLUMN IF NOT EXISTS prioritized boolean NOT NULL DEFAULT false",
    "ALTER TABLE derivation_build ADD COLUMN IF NOT EXISTS prioritized boolean NOT NULL DEFAULT false",
    "CREATE OR REPLACE FUNCTION unprioritize() RETURNS trigger \
     LANGUAGE plpgsql AS $$ BEGIN NEW.prioritized := false; RETURN NEW; END $$",
    "CREATE OR REPLACE TRIGGER derivation_build_unprioritize BEFORE UPDATE OF status ON derivation_build \
     FOR EACH ROW WHEN (NEW.prioritized AND NEW.status IN (4, 5, 6, 9, 10)) \
     EXECUTE FUNCTION unprioritize()",
    "CREATE OR REPLACE TRIGGER evaluation_unprioritize BEFORE UPDATE OF status ON evaluation \
     FOR EACH ROW WHEN (NEW.prioritized AND NEW.status IN (6, 7)) \
     EXECUTE FUNCTION unprioritize()",
];

const DOWN: &[&str] = &[
    "DROP TRIGGER IF EXISTS evaluation_unprioritize ON evaluation",
    "DROP TRIGGER IF EXISTS derivation_build_unprioritize ON derivation_build",
    "DROP FUNCTION IF EXISTS unprioritize()",
    "ALTER TABLE derivation_build DROP COLUMN IF EXISTS prioritized",
    "ALTER TABLE evaluation DROP COLUMN IF EXISTS prioritized",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in UP {
            manager.get_connection().execute_unprepared(stmt).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in DOWN {
            manager.get_connection().execute_unprepared(stmt).await?;
        }

        Ok(())
    }
}
