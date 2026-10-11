/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("cache")]
#[derive(Args, Debug, Clone)]
pub struct CacheArgs {
    /// Maximum simultaneous narinfo requests to upstream caches across the server.
    #[arg(default_value_t = 32)]
    pub upstream_query_concurrency: usize,

    /// Instance-wide limit on cached NAR storage in GB. New evaluations wait while every
    /// writable cache of a project has less than 10 MiB left. Set it to `0` to disable the
    /// limit. Per-cache limits still apply.
    #[arg(default_value_t = 0)]
    pub max_storage_gb: i32,

    /// Seconds between NAR signature backfill passes. Uploads receive their signature
    /// immediately. Only subscription placeholders and unsigned leftovers need the pass.
    #[arg(default_value_t = 3600)]
    pub sign_sweep_interval_secs: u64,

    /// Seconds between passes of the DWARF build ID index backfill. Uploads enter the index
    /// immediately. Only paths cached before the index existed or interrupted by a restart need
    /// the pass.
    #[arg(default_value_t = 300)]
    pub debug_index_interval_secs: u64,
}
