/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Status reports moved to the typed event row; a report still pending at upgrade
//! is settled with a reason instead of being read by a consumer that no longer knows its shape.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &["UPDATE outbox SET failed_at = (now() AT TIME ZONE 'UTC'), \
         last_error = 'superseded by the typed event outbox' \
     WHERE kind IN (0, 1) AND delivered_at IS NULL AND failed_at IS NULL"];

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

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP;

    #[test]
    fn legacy_status_rows_are_settled_not_dropped() {
        assert!(
            UP.iter()
                .any(|s| s.contains("kind IN (0, 1)") && s.contains("failed_at"))
        );
    }
}
