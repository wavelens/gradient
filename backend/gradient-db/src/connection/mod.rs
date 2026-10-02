/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::permissions::{
    admin_mask, cache_admin_mask, cache_view_mask, cache_write_mask, view_mask, write_mask,
};
use anyhow::{Context, Result};
use gradient_migration::Migrator;
use gradient_types::consts::{
    BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID, BASE_CACHE_ROLE_WRITE_ID,
    BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID,
};
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ConnectOptions, ConnectionTrait, Database,
    DatabaseConnection, DbErr, EntityTrait, IntoActiveModel, Value,
};
use sea_orm_migration::prelude::*;
use std::time::Duration;
use tracing::log::LevelFilter;

fn db_url(cli: &Cli) -> Result<String> {
    if let Some(file) = &cli.database.url_file {
        Ok(std::fs::read_to_string(file).context("Failed to read database url from file")?)
    } else if let Some(url) = &cli.database.url {
        Ok(url.clone())
    } else {
        anyhow::bail!("No database url provided")
    }
}

fn make_connect_options(
    cli: &Cli,
    max_connections: u32,
    min_connections: u32,
) -> Result<ConnectOptions> {
    let mut opt = ConnectOptions::new(db_url(cli)?);

    if cli.log.level_default == "trace" {
        opt.sqlx_logging(true)
            .sqlx_logging_level(LevelFilter::Trace);
    } else {
        opt.sqlx_logging(false);
    }

    opt.max_connections(max_connections)
        .min_connections(min_connections)
        .connect_timeout(Duration::from_secs(8))
        .acquire_timeout(Duration::from_secs(8))
        .idle_timeout(Duration::from_secs(600))
        .max_lifetime(Duration::from_secs(1800));

    Ok(opt)
}

const MIN_SERVER_VERSION_NUM: i32 = 180_000;

fn require_supported_pg_version(server_version_num: i32) -> Result<()> {
    if server_version_num < MIN_SERVER_VERSION_NUM {
        anyhow::bail!(
            "Gradient requires PostgreSQL 18 or newer (uuidv7); connected server is {}.{}",
            server_version_num / 10_000,
            server_version_num % 10_000,
        );
    }

    Ok(())
}

const MIN_MAX_LOCKS_PER_TRANSACTION: i32 = 256;

fn lock_table_warning(max_locks_per_transaction: i32) -> Option<String> {
    (max_locks_per_transaction < MIN_MAX_LOCKS_PER_TRANSACTION).then(|| {
        format!(
            "PostgreSQL runs with max_locks_per_transaction = {max_locks_per_transaction}; \
             graph writes hold one advisory lock per shared build and dependency they count, and a \
             record batch can exhaust the lock table (\"out of shared memory\"). Set it to at \
             least {MIN_MAX_LOCKS_PER_TRANSACTION} (the NixOS module sets 1024)."
        )
    })
}

crate::sql! {
    SERVER_VERSION_NUM = "SELECT current_setting('server_version_num')::int4 AS v",
        params = [];

    MAX_LOCKS_PER_TRANSACTION = "SELECT current_setting('max_locks_per_transaction')::int4 AS v",
        params = [];
}

/// This check is only warning, never failing.
/// A small lock table is only hurting a large enough batch.
/// The fix is a Postgres restart the operator must schedule.
async fn check_lock_table(db: &DatabaseConnection) {
    let setting = db
        .query_one_raw(MAX_LOCKS_PER_TRANSACTION.stmt())
        .await
        .ok()
        .flatten()
        .and_then(|row| row.try_get::<i32>("", "v").ok());
    match setting {
        Some(v) => {
            if let Some(warning) = lock_table_warning(v) {
                tracing::error!("{warning}");
            }
        }
        None => tracing::warn!("could not read max_locks_per_transaction"),
    }
}

async fn server_version_num(db: &DatabaseConnection) -> Result<i32> {
    db.query_one_raw(SERVER_VERSION_NUM.stmt())
        .await
        .context("Failed to query PostgreSQL server version")?
        .context("PostgreSQL returned no server_version_num")?
        .try_get::<i32>("", "v")
        .context("Failed to read server_version_num")
}

pub async fn connect_db(cli: &Cli) -> Result<DatabaseConnection> {
    let db = Database::connect(make_connect_options(
        cli,
        cli.database.max_connections,
        cli.database.min_connections,
    )?)
    .await
    .context("Failed to connect to database")?;
    require_supported_pg_version(server_version_num(&db).await?)?;
    check_lock_table(&db).await;
    Migrator::install(&db)
        .await
        .context("Failed to install seaql_migrations table")?;
    prune_removed_migrations(&db)
        .await
        .context("Failed to prune removed-migration entries from seaql_migrations")?;
    run_migrations(&db).await?;
    update_db(&db).await.context("Failed to update database")?;
    Ok(db)
}

async fn run_migrations(db: &DatabaseConnection) -> Result<()> {
    let pending = Migrator::get_pending_migrations(db)
        .await
        .context("Failed to determine pending database migrations")?;
    if pending.is_empty() {
        tracing::info!("database schema up to date; no migrations to run");
        return Ok(());
    }

    let names: Vec<&str> = pending.iter().map(|m| m.name()).collect();
    tracing::info!(count = pending.len(), migrations = ?names, "running database migrations; this may take a while");
    Migrator::up(db, None)
        .await
        .context("Failed to run database migrations")?;
    tracing::info!(count = pending.len(), "database migrations complete");

    Ok(())
}

fn prune_removed_migrations_sql(known: &[Value]) -> String {
    let placeholders: Vec<String> = (1..=known.len()).map(|i| format!("${i}")).collect();
    format!(
        "DELETE FROM seaql_migrations WHERE version NOT IN ({}) RETURNING version",
        placeholders.join(", ")
    )
}

crate::sql_fn! {
    PRUNE_REMOVED_MIGRATIONS = || {
        let known = [
            Value::from("m20260619_010000_globalize_derivation".to_string()),
            Value::from("m20260619_020000_derivation_build_anchor".to_string()),
        ];
        prune_removed_migrations_sql(&known)
    },
        params = [
            Text("m20260619_010000_globalize_derivation"),
            Text("m20260619_020000_derivation_build_anchor"),
        ];
}

async fn prune_removed_migrations(db: &DatabaseConnection) -> Result<()> {
    let known: Vec<Value> = Migrator::migrations()
        .iter()
        .map(|m| Value::from(m.name().to_string()))
        .collect();
    if known.is_empty() {
        return Ok(());
    }
    let rows = db
        .query_all_raw(
            PRUNE_REMOVED_MIGRATIONS.bind_built(prune_removed_migrations_sql(&known), known),
        )
        .await?;
    if !rows.is_empty() {
        let pruned: Vec<String> = rows
            .iter()
            .filter_map(|r| r.try_get::<String>("", "version").ok())
            .collect();
        tracing::info!(?pruned, "pruned orphan seaql_migrations rows");
    }
    Ok(())
}

pub async fn connect_web_db(cli: &Cli) -> Result<DatabaseConnection> {
    Database::connect(make_connect_options(
        cli,
        cli.database.web_max_connections,
        cli.database.web_min_connections,
    )?)
    .await
    .context("Failed to connect web database pool")
}

pub async fn connect_cache_db(cli: &Cli) -> Result<DatabaseConnection> {
    Database::connect(make_connect_options(
        cli,
        cli.database.cache_max_connections,
        cli.database.cache_min_connections,
    )?)
    .await
    .context("Failed to connect cache database pool")
}

/// Restart recovery must not run here.
/// It once aborted the evaluations before `recover_interrupted_work` started.
/// Their shared builds then stayed `Created` or `Queued` forever.
async fn update_db(db: &DatabaseConnection) -> Result<(), DbErr> {
    seed_builtin_role(db, BASE_ROLE_ADMIN_ID, "Admin", admin_mask()).await?;
    seed_builtin_role(db, BASE_ROLE_WRITE_ID, "Write", write_mask()).await?;
    seed_builtin_role(db, BASE_ROLE_VIEW_ID, "View", view_mask()).await?;

    seed_builtin_cache_role(db, BASE_CACHE_ROLE_ADMIN_ID, "Admin", cache_admin_mask()).await?;
    seed_builtin_cache_role(db, BASE_CACHE_ROLE_WRITE_ID, "Write", cache_write_mask()).await?;
    seed_builtin_cache_role(db, BASE_CACHE_ROLE_VIEW_ID, "View", cache_view_mask()).await?;

    Ok(())
}

async fn seed_builtin_role(
    db: &DatabaseConnection,
    role_id: RoleId,
    name: &str,
    permission: i64,
) -> Result<(), DbErr> {
    match ERole::find_by_id(role_id).one(db).await? {
        None => {
            MRole {
                id: role_id,
                name: name.to_string(),
                permission,
                ..Default::default()
            }
            .into_active_model()
            .insert(db)
            .await?;
        }
        Some(existing) if existing.permission != permission || existing.name != name => {
            let mut active: ARole = existing.into();
            active.name = Set(name.to_string());
            active.permission = Set(permission);
            active.update(db).await?;
        }
        Some(_) => {}
    }
    Ok(())
}

async fn seed_builtin_cache_role(
    db: &DatabaseConnection,
    role_id: RoleId,
    name: &str,
    permission: i64,
) -> Result<(), DbErr> {
    match ECacheRole::find_by_id(role_id).one(db).await? {
        None => {
            MCacheRole {
                id: role_id,
                name: name.to_string(),
                permission,
                ..Default::default()
            }
            .into_active_model()
            .insert(db)
            .await?;
        }
        Some(existing) if existing.permission != permission || existing.name != name => {
            let mut active: ACacheRole = existing.into();
            active.name = Set(name.to_string());
            active.permission = Set(permission);
            active.update(db).await?;
        }
        Some(_) => {}
    }
    Ok(())
}

#[cfg(test)]
mod pg_version_tests {
    use super::require_supported_pg_version;

    #[test]
    fn rejects_postgres_below_18() {
        let err = require_supported_pg_version(170_004)
            .unwrap_err()
            .to_string();
        assert!(err.contains("PostgreSQL 18"), "{err}");
        assert!(err.contains("17.4"), "{err}");
    }

    #[test]
    fn rejects_one_below_the_minimum() {
        assert!(require_supported_pg_version(179_999).is_err());
    }

    #[test]
    fn accepts_postgres_18_and_newer() {
        assert!(require_supported_pg_version(180_000).is_ok());
        assert!(require_supported_pg_version(190_002).is_ok());
    }
}

#[cfg(test)]
mod lock_table_tests {
    use super::lock_table_warning;

    #[test]
    fn a_lock_table_below_the_floor_names_the_setting_and_the_fix() {
        let w = lock_table_warning(64).expect("64 is below the floor");
        assert!(w.contains("max_locks_per_transaction = 64"), "{w}");
        assert!(w.contains("256"), "{w}");
        assert!(lock_table_warning(256).is_none());
        assert!(lock_table_warning(1024).is_none());
    }
}
