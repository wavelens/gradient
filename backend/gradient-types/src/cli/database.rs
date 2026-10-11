/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("database")]
#[derive(Args, Debug, Clone)]
pub struct DatabaseArgs {
    pub url: Option<String>,
    pub url_file: Option<String>,

    /// Maximum connections the scheduler / worker pool may open.
    /// Total Postgres connections per gradient-server process is
    /// `max_connections + web_max_connections + cache_max_connections`.
    #[arg(id = "database-max-connections", default_value_t = 32)]
    pub max_connections: u32,

    /// Minimum connections kept warm in the scheduler / worker pool.
    #[arg(default_value_t = 2)]
    pub min_connections: u32,

    /// Maximum connections the cache-query pool may open. A dedicated pool is keeping a large
    /// eval's worker prefetch storm from starving the scheduler/worker pool and stalling dispatch.
    /// The storm is one `CacheQuery` per in-flight build.
    #[arg(default_value_t = 32)]
    pub cache_max_connections: u32,

    /// Minimum connections kept warm in the cache-query pool.
    #[arg(default_value_t = 2)]
    pub cache_min_connections: u32,

    /// Maximum connections the axum HTTP pool may open.
    #[arg(default_value_t = 16)]
    pub web_max_connections: u32,

    /// Minimum connections kept warm in the axum HTTP pool.
    #[arg(default_value_t = 1)]
    pub web_min_connections: u32,
}
