/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct UploadArgs {
    /// Uploads (NARs and eval-cache blobs) admitted at once across every worker
    /// and REST client; further requests wait for a permit.
    #[arg(
        long = "upload-concurrency",
        env = "GRADIENT_UPLOAD_CONCURRENCY",
        default_value_t = 16
    )]
    pub concurrency: usize,

    /// Sum of admitted upload sizes; a request that does not fit waits, and one
    /// larger than the whole budget runs alone once nothing else is in flight.
    #[arg(long = "upload-bytes-budget", env = "GRADIENT_UPLOAD_BYTES_BUDGET", default_value_t = 8 * 1024 * 1024 * 1024)]
    pub bytes_budget: u64,

    /// Seconds a granted relay upload may go without a chunk before its permit
    /// is reclaimed and the worker is told to retry.
    #[arg(
        long = "upload-lease-idle-secs",
        env = "GRADIENT_UPLOAD_LEASE_IDLE_SECS",
        default_value_t = 300
    )]
    pub lease_idle_secs: u64,
}

impl Default for UploadArgs {
    fn default() -> Self {
        Self {
            concurrency: 16,
            bytes_budget: 8 * 1024 * 1024 * 1024,
            lease_idle_secs: 300,
        }
    }
}
