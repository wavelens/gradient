/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::disallowed_methods,
    reason = "migrations are not independently executable"
)]

pub use sea_orm_migration::prelude::*;

mod m20261013_000000_baseline;
mod m20261014_000000_evaluation_branch;
mod m20261014_000001_auto_concurrency;
mod m20261015_000000_evaluation_metric_per_evaluation;
mod m20261015_000001_output_cache_by_hash;
mod m20261015_000002_pull_request_summary;

pub struct UpgradeFloor {
    pub release: &'static str,
    pub last_migration: &'static str,
    pub storage_migrations: &'static [&'static str],
}

pub const UPGRADE_FLOOR: UpgradeFloor = UpgradeFloor {
    release: "v2.0.0",
    last_migration: "m20261012_000001_wildcard_grammar",
    storage_migrations: &["m20261001_000000_shard_build_logs"],
};

/// The SQL of a function created by the baseline. Tests compare it with the Rust side.
pub fn function_sql(name: &str) -> &'static str {
    let sql = include_str!("m20261013_000000_baseline.sql");
    let marker = format!("CREATE FUNCTION public.{name}(");
    let start = sql.find(&marker).expect("the function in the baseline");
    let end = start + sql[start..].find("$$;").expect("the function end") + 3;
    &sql[start..end]
}

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261013_000000_baseline::Migration),
            Box::new(m20261014_000000_evaluation_branch::Migration),
            Box::new(m20261014_000001_auto_concurrency::Migration),
            Box::new(m20261015_000000_evaluation_metric_per_evaluation::Migration),
            Box::new(m20261015_000001_output_cache_by_hash::Migration),
            Box::new(m20261015_000002_pull_request_summary::Migration),
        ]
    }
}
