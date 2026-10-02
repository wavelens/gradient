/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct UploadArgs {
    /// Uploads over 1 MiB (NARs and eval cache blobs) admitted at once across all workers and REST
    /// clients. Smaller uploads have a window of 128 of their own. An upload is holding its permit
    /// until the object is in storage. Further uploads are waiting for a permit.
    #[arg(
        long = "upload-concurrency",
        env = "GRADIENT_UPLOAD_CONCURRENCY",
        default_value_t = 16
    )]
    pub concurrency: usize,

    /// Total size in bytes of admitted uploads. An upload not fitting the remaining budget is
    /// waiting. An upload larger than the whole budget is running alone once nothing else is in
    /// flight.
    #[arg(long = "upload-bytes-budget", env = "GRADIENT_UPLOAD_BYTES_BUDGET", default_value_t = 8 * 1024 * 1024 * 1024)]
    pub bytes_budget: u64,

    /// Seconds a granted worker upload may go without data before the server is reclaiming its
    /// permit and telling the worker to retry.
    #[arg(
        long = "upload-lease-idle-secs",
        env = "GRADIENT_UPLOAD_LEASE_IDLE_SECS",
        default_value_t = 300
    )]
    pub lease_idle_secs: u64,

    /// Seconds a NAR upload to the cache upload endpoint is waiting for a permit before the server
    /// is answering with 503 and `Retry-After`.
    #[arg(
        long = "upload-rest-wait-secs",
        env = "GRADIENT_UPLOAD_REST_WAIT_SECS",
        default_value_t = 30
    )]
    pub rest_wait_secs: u64,
}

impl Default for UploadArgs {
    fn default() -> Self {
        Self {
            concurrency: 16,
            bytes_budget: 8 * 1024 * 1024 * 1024,
            lease_idle_secs: 300,
            rest_wait_secs: 30,
        }
    }
}
