/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct DatabaseArgs {
    #[arg(long = "database-url", env = "GRADIENT_DATABASE_URL")]
    pub url: Option<String>,
    #[arg(long = "database-url-file", env = "GRADIENT_DATABASE_URL_FILE")]
    pub url_file: Option<String>,

    /// Maximum connections the scheduler / worker pool may open.
    /// Total Postgres connections per gradient-server process is
    /// `max_connections + web_max_connections + cache_max_connections`.
    #[arg(
        id = "database-max-connections",
        long = "database-max-connections",
        env = "GRADIENT_DATABASE_MAX_CONNECTIONS",
        default_value_t = 32
    )]
    pub max_connections: u32,

    /// Minimum connections kept warm in the scheduler / worker pool.
    #[arg(
        long = "database-min-connections",
        env = "GRADIENT_DATABASE_MIN_CONNECTIONS",
        default_value_t = 2
    )]
    pub min_connections: u32,

    /// Maximum connections the cache-query pool may open. A dedicated pool is keeping a large
    /// eval's worker prefetch storm from starving the scheduler/worker pool and stalling dispatch.
    /// The storm is one `CacheQuery` per in-flight build.
    #[arg(
        long = "database-cache-max-connections",
        env = "GRADIENT_DATABASE_CACHE_MAX_CONNECTIONS",
        default_value_t = 32
    )]
    pub cache_max_connections: u32,

    /// Minimum connections kept warm in the cache-query pool.
    #[arg(
        long = "database-cache-min-connections",
        env = "GRADIENT_DATABASE_CACHE_MIN_CONNECTIONS",
        default_value_t = 2
    )]
    pub cache_min_connections: u32,

    /// Maximum connections the axum HTTP pool may open.
    #[arg(
        long = "database-web-max-connections",
        env = "GRADIENT_DATABASE_WEB_MAX_CONNECTIONS",
        default_value_t = 16
    )]
    pub web_max_connections: u32,

    /// Minimum connections kept warm in the axum HTTP pool.
    #[arg(
        long = "database-web-min-connections",
        env = "GRADIENT_DATABASE_WEB_MIN_CONNECTIONS",
        default_value_t = 1
    )]
    pub web_min_connections: u32,
}

impl Default for DatabaseArgs {
    fn default() -> Self {
        Self {
            url: None,
            url_file: None,
            max_connections: 32,
            min_connections: 2,
            cache_max_connections: 32,
            cache_min_connections: 2,
            web_max_connections: 16,
            web_min_connections: 1,
        }
    }
}
