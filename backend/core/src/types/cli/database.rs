/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct DatabaseArgs {
    #[arg(long, env = "GRADIENT_DATABASE_URL")]
    pub database_url: Option<String>,
    #[arg(long, env = "GRADIENT_DATABASE_URL_FILE")]
    pub database_url_file: Option<String>,

    /// Maximum connections the scheduler / worker / cache pool may open.
    /// Total Postgres connections per gradient-server process is
    /// `database_max_connections + database_web_max_connections`.
    #[arg(
        long,
        env = "GRADIENT_DATABASE_MAX_CONNECTIONS",
        default_value_t = 32
    )]
    pub database_max_connections: u32,

    /// Minimum connections kept warm in the scheduler / worker / cache pool.
    #[arg(
        long,
        env = "GRADIENT_DATABASE_MIN_CONNECTIONS",
        default_value_t = 2
    )]
    pub database_min_connections: u32,

    /// Maximum connections the axum HTTP pool may open.
    #[arg(
        long,
        env = "GRADIENT_DATABASE_WEB_MAX_CONNECTIONS",
        default_value_t = 16
    )]
    pub database_web_max_connections: u32,

    /// Minimum connections kept warm in the axum HTTP pool.
    #[arg(
        long,
        env = "GRADIENT_DATABASE_WEB_MIN_CONNECTIONS",
        default_value_t = 1
    )]
    pub database_web_min_connections: u32,
}

impl Default for DatabaseArgs {
    fn default() -> Self {
        Self {
            database_url: None,
            database_url_file: None,
            database_max_connections: 32,
            database_min_connections: 2,
            database_web_max_connections: 16,
            database_web_min_connections: 1,
        }
    }
}
