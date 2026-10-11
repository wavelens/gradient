/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::num::NonZeroU32;

use clap::Args;
use gradient_derive::config_group;

#[config_group("database")]
#[derive(Args, Debug, Clone)]
pub struct DatabaseArgs {
    pub url: Option<String>,
    pub url_file: Option<String>,

    /// Maximum connections of the scheduler and worker pool. Server processes each open up to
    /// the sum of {option}`services.gradient.database.maxConnections`,
    /// {option}`services.gradient.database.cache.maxConnections` and
    /// {option}`services.gradient.database.web.maxConnections`. PostgreSQL's `max_connections`
    /// must accommodate that sum.
    #[arg(id = "database-max-connections", default_value = "16")]
    pub max_connections: NonZeroU32,

    /// Minimum connections kept open in the scheduler and worker pool.
    #[arg(default_value_t = 2)]
    pub min_connections: u32,

    /// Maximum connections of the cache query pool. It is separate from the scheduler pool to
    /// keep a large evaluation's prefetch traffic from starving job assignment.
    #[arg(default_value = "32")]
    pub cache_max_connections: NonZeroU32,

    /// Minimum connections kept open in the cache query pool.
    #[arg(default_value_t = 2)]
    pub cache_min_connections: u32,

    /// Maximum connections of the HTTP API pool.
    #[arg(default_value = "8")]
    pub web_max_connections: NonZeroU32,

    /// Minimum connections kept open in the HTTP API pool.
    #[arg(default_value_t = 1)]
    pub web_min_connections: u32,
}
