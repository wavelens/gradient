/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Storage migrations are to the NAR, log and blob layout what database migrations are to the
//! schema. They must apply in order and before any deep GC unit. The deep GC is reading only the
//! layout they leave behind.

mod m20261001_000000_shard_build_logs;

use crate::units::{Step, next_unit};
use anyhow::{Context, Result};
use futures::future::BoxFuture;
use gradient_core::ServerState;
use gradient_db::maintenance::storage_migrations;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::info;

/// Every unit must be idempotent. A unit cut short by a restart is starting again in full.
pub(crate) trait StorageMigration: Send + Sync {
    fn name(&self) -> &'static str;

    fn units(&self) -> Vec<String>;

    fn migrate<'a>(&'a self, state: &'a ServerState, unit: &'a str) -> BoxFuture<'a, Result<()>>;
}

fn migrations() -> Vec<Box<dyn StorageMigration>> {
    vec![Box::new(m20261001_000000_shard_build_logs::Migration)]
}

pub(super) async fn step(state: &Arc<ServerState>) -> Result<Step> {
    let ledger = storage_migrations::all(&state.worker_db).await?;
    let applied: HashSet<&str> = ledger
        .iter()
        .filter(|m| m.applied_at.is_some())
        .map(|m| m.name.as_str())
        .collect();
    let Some(migration) = migrations()
        .into_iter()
        .find(|m| !applied.contains(m.name()))
    else {
        return Ok(Step::Idle);
    };

    let name = migration.name();
    let checkpoint = ledger
        .iter()
        .find(|m| m.name == name)
        .and_then(|m| m.checkpoint.as_deref());
    let units = migration.units();
    match next_unit(&units, checkpoint) {
        Some(unit) => {
            migration
                .migrate(state, unit)
                .await
                .with_context(|| format!("storage migration {name}, unit {unit}"))?;
            storage_migrations::save_checkpoint(&state.worker_db, name, unit).await?;
        }
        None => {
            storage_migrations::mark_applied(&state.worker_db, name).await?;
            info!(migration = name, "storage migration applied");
        }
    }

    Ok(Step::Paced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_server_state;
    use gradient_storage::NarStore;
    use gradient_types::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[tokio::test]
    async fn applied_migrations_leave_the_step_to_the_deep_gc() {
        let applied: Vec<MStorageMigration> = migrations()
            .iter()
            .map(|m| MStorageMigration {
                name: m.name().to_owned(),
                applied_at: Some(now()),
                ..Default::default()
            })
            .collect();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([applied])
            .into_connection();
        let tmp = tempfile::tempdir().unwrap();
        let nar = NarStore::local(tmp.path().to_str().unwrap()).unwrap();

        let state = test_server_state(nar, db, |_| {});
        assert_eq!(step(&state).await.unwrap(), Step::Idle);
    }
}
