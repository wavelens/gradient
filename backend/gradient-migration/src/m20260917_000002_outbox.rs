/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The effects outbox: what a state change owes the outside world, written in
//! the transaction that made the change, delivered later, retried with backoff
//! and dead-lettered in place.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS outbox ( \
        id uuid PRIMARY KEY, \
        kind smallint NOT NULL, \
        key text NOT NULL, \
        payload jsonb NOT NULL, \
        created_at timestamp NOT NULL, \
        attempts integer NOT NULL DEFAULT 0, \
        next_attempt_at timestamp NOT NULL, \
        delivered_at timestamp, \
        failed_at timestamp, \
        last_error text)",
    "CREATE INDEX IF NOT EXISTS \"idx-outbox-due\" ON outbox (next_attempt_at) \
     WHERE delivered_at IS NULL AND failed_at IS NULL",
    "CREATE UNIQUE INDEX IF NOT EXISTS \"idx-outbox-key\" ON outbox (kind, key) \
     WHERE delivered_at IS NULL AND failed_at IS NULL",
    "CREATE INDEX IF NOT EXISTS \"idx-outbox-settled\" ON outbox (coalesce(delivered_at, failed_at)) \
     WHERE delivered_at IS NOT NULL OR failed_at IS NOT NULL",
];

const DOWN: &str = "DROP TABLE IF EXISTS outbox";

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
        manager.get_connection().execute_unprepared(DOWN).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP;

    /// The claim reads due rows by `next_attempt_at` and the enqueue folds a
    /// duplicate into the pending row; both need their partial index, and both
    /// predicates must name the same "still owed" rows.
    #[test]
    fn the_pending_indexes_agree_on_what_is_still_owed() {
        let pending: Vec<&str> = UP
            .iter()
            .filter(|s| s.contains("WHERE delivered_at IS NULL AND failed_at IS NULL"))
            .copied()
            .collect();

        assert_eq!(pending.len(), 2, "{pending:?}");
        assert!(pending.iter().any(|s| s.contains("(next_attempt_at)")));
        assert!(
            pending
                .iter()
                .any(|s| s.contains("UNIQUE") && s.contains("(kind, key)"))
        );
    }
}
