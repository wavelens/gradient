/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("upload")]
#[derive(Args, Debug, Clone)]
pub struct UploadArgs {
    /// Uploads over 1 MiB (NARs and eval cache blobs) admitted at once across all workers and REST
    /// clients. Smaller uploads have a window of 128 of their own. An upload is holding its permit
    /// until the object is in storage. Further uploads are waiting for a permit.
    #[arg(default_value_t = 8)]
    pub concurrency: usize,

    /// Total size in bytes of admitted uploads. An upload not fitting the remaining budget is
    /// waiting. An upload larger than the whole budget is running alone once nothing else is in
    /// flight.
    #[arg(default_value_t = 8 * 1024 * 1024 * 1024)]
    pub bytes_budget: u64,

    /// Seconds a granted worker upload may go without data before the server is reclaiming its
    /// permit and telling the worker to retry.
    #[arg(default_value_t = 300)]
    pub lease_idle_secs: u64,

    /// Seconds a NAR upload to the cache upload endpoint is waiting for a permit before the server
    /// is answering with 503 and `Retry-After`.
    #[arg(default_value_t = 30)]
    pub rest_wait_secs: u64,
}
