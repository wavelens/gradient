/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Storage migrations are to the NAR, log and blob layout what database migrations are to the
//! schema. Apply them in order and before any deep GC unit.

mod eval_cache_per_cache;
mod eval_cache_segments;

use crate::units::{Step, next_unit};
use anyhow::{Context, Result};
use futures::future::BoxFuture;
use gradient_db::DbContext;
use gradient_db::maintenance::storage_migrations;
use std::collections::HashSet;
use tracing::info;

/// Keep every unit idempotent. Apply a cut unit again in full after a restart.
pub(crate) trait StorageMigration: Send + Sync {
    fn name(&self) -> &'static str;

    fn units(&self) -> Vec<String>;

    fn migrate<'a>(&'a self, db: &'a DbContext, unit: &'a str) -> BoxFuture<'a, Result<()>>;
}

fn migrations() -> Vec<Box<dyn StorageMigration>> {
    vec![
        Box::new(eval_cache_per_cache::EvalCachePerCache),
        Box::new(eval_cache_segments::EvalCacheSegments),
    ]
}

pub(super) async fn step(db: &DbContext) -> Result<Step> {
    let ledger = storage_migrations::all(&db.worker_db).await?;
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
                .migrate(db, unit)
                .await
                .with_context(|| format!("storage migration {name}, unit {unit}"))?;
            storage_migrations::save_checkpoint(&db.worker_db, name, unit).await?;
        }
        None => {
            storage_migrations::mark_applied(&db.worker_db, name).await?;
            info!(migration = name, "storage migration applied");
        }
    }

    Ok(Step::Paced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_server_state;
    use gradient_fixtures::mock::mock_db;
    use gradient_storage::NarStore;
    use gradient_types::*;

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
        let db = mock_db().append_query_results([applied]).into_connection();
        let tmp = tempfile::tempdir().unwrap();
        let nar = NarStore::local(tmp.path()).unwrap();

        let state = test_server_state(nar, db, |_| {});
        assert_eq!(step(&state.db()).await.unwrap(), Step::Idle);
    }
}
