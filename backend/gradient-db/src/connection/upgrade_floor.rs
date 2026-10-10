/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::sql::Query;
use anyhow::{Context, Result};
use gradient_migration::{Migrator, UPGRADE_FLOOR, UpgradeFloor};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use sea_orm_migration::MigratorTrait;
use std::collections::HashSet;

crate::sql! {
    APPLIED_MIGRATIONS = "SELECT version AS name FROM seaql_migrations",
        params = [];

    FINISHED_STORAGE_MIGRATIONS =
        "SELECT name FROM storage_migration WHERE applied_at IS NOT NULL",
        params = [];
}

pub(super) async fn require_upgrade_floor(db: &DatabaseConnection) -> Result<()> {
    let applied = names(db, &APPLIED_MIGRATIONS)
        .await
        .context("Failed to list the applied database migrations")?;

    if !is_upgrade_from_an_earlier_major(&applied, &baseline()?) {
        return Ok(());
    }

    require_last_migration(&UPGRADE_FLOOR, &applied)?;
    if UPGRADE_FLOOR.storage_migrations.is_empty() {
        return Ok(());
    }

    let finished = names(db, &FINISHED_STORAGE_MIGRATIONS)
        .await
        .context("Failed to list the finished storage migrations")?;

    require_storage_migrations(&UPGRADE_FLOOR, &finished)
}

fn baseline() -> Result<String> {
    Migrator::migrations()
        .first()
        .map(|migration| migration.name().to_owned())
        .context("The migration list holds no baseline")
}

async fn names(db: &DatabaseConnection, query: &Query) -> Result<HashSet<String>> {
    let rows = db.query_all_raw(query.stmt()).await?;
    let names = rows
        .iter()
        .map(|row| row.try_get::<String>("", "name"))
        .collect::<Result<_, _>>()?;

    Ok(names)
}

fn is_upgrade_from_an_earlier_major(applied: &HashSet<String>, baseline: &str) -> bool {
    !applied.is_empty() && !applied.contains(baseline)
}

fn require_last_migration(floor: &UpgradeFloor, applied: &HashSet<String>) -> Result<()> {
    if applied.contains(floor.last_migration) {
        return Ok(());
    }

    anyhow::bail!(
        "the database is behind the upgrade floor: migration {} of {} is not applied. Run {} \
         until its database and storage migrations finish, then start this release again",
        floor.last_migration,
        floor.release,
        floor.release,
    )
}

fn require_storage_migrations(floor: &UpgradeFloor, finished: &HashSet<String>) -> Result<()> {
    let unfinished: Vec<&str> = floor
        .storage_migrations
        .iter()
        .copied()
        .filter(|name| !finished.contains(*name))
        .collect();

    if unfinished.is_empty() {
        return Ok(());
    }

    anyhow::bail!(
        "the storage is behind the upgrade floor: storage migrations {} of {} are not finished. \
         Run {} until it logs \"storage migration applied\" for each of them, then start this \
         release again",
        unfinished.join(", "),
        floor.release,
        floor.release,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLOOR: UpgradeFloor = UpgradeFloor {
        release: "v9.0.0",
        last_migration: "m3_last",
        storage_migrations: &["m1_shard_logs", "m2_shard_nars"],
    };

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn a_fresh_database_is_no_upgrade() {
        assert!(!is_upgrade_from_an_earlier_major(&set(&[]), "m4_baseline"));
    }

    #[test]
    fn a_database_holding_the_baseline_is_no_upgrade() {
        let applied = set(&["m4_baseline", "m5_later"]);
        assert!(!is_upgrade_from_an_earlier_major(&applied, "m4_baseline"));
    }

    #[test]
    fn a_database_of_an_earlier_baseline_is_an_upgrade() {
        let applied = set(&["m0_baseline", "m3_last"]);
        assert!(is_upgrade_from_an_earlier_major(&applied, "m4_baseline"));
    }

    #[test]
    fn a_database_behind_the_last_migration_is_refused_with_the_release_to_run() {
        let err = require_last_migration(&FLOOR, &set(&["m0_baseline", "m2_earlier"]))
            .unwrap_err()
            .to_string();

        assert!(err.contains("m3_last"), "{err}");
        assert!(err.contains("Run v9.0.0"), "{err}");
        assert!(require_last_migration(&FLOOR, &set(&["m0_baseline", "m3_last"])).is_ok());
    }

    #[test]
    fn an_unfinished_storage_migration_is_refused_by_name() {
        let err = require_storage_migrations(&FLOOR, &set(&["m1_shard_logs"]))
            .unwrap_err()
            .to_string();

        assert!(err.contains("m2_shard_nars"), "{err}");
        assert!(!err.contains("m1_shard_logs"), "{err}");
        assert!(err.contains("Run v9.0.0"), "{err}");
        assert!(
            require_storage_migrations(&FLOOR, &set(&["m1_shard_logs", "m2_shard_nars"])).is_ok()
        );
    }
}
