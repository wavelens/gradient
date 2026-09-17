/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `derivation_build.demanded`: something still wants this anchor's outputs in
//! our cache. Defaulting true keeps every pending anchor promotable across the
//! deploy; the consistency sweep's absolute recompute is the backfill.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "ALTER TABLE derivation_build ADD COLUMN IF NOT EXISTS demanded boolean NOT NULL DEFAULT true",
];

const DOWN: &[&str] = &["ALTER TABLE derivation_build DROP COLUMN IF EXISTS demanded"];

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

#[cfg(test)]
mod tests {
    use super::{DOWN, UP};

    /// A stale `false` stalls every pending build; a stale `true` only wastes work
    /// the fleet was already doing. The consistency sweep recomputes the column
    /// absolutely on its first pass, so the default IS the backfill and no second
    /// copy of the definition has to live in this crate.
    #[test]
    fn every_existing_row_is_demanded_by_the_default() {
        assert!(UP[0].contains("NOT NULL DEFAULT true"), "{}", UP[0]);
        assert_eq!(
            UP.len(),
            1,
            "the sweep is the backfill, not a statement here"
        );
    }

    #[test]
    fn down_removes_exactly_what_up_added() {
        assert!(
            DOWN[0].contains("DROP COLUMN IF EXISTS demanded"),
            "{}",
            DOWN[0]
        );
    }
}
