/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::num::{NonZeroU64, NonZeroUsize};

use clap::Args;
use gradient_derive::config_group;

#[config_group("upload")]
#[derive(Args, Debug, Clone)]
pub struct UploadArgs {
    /// The number of uploads over 1 MiB (NAR and eval cache segments) that run concurrently across
    /// all workers, REST clients and SSH `nix copy` clients. Smaller uploads have a dedicated pool
    /// of 128 slots. Uploads count against this limit until the object reaches storage. Other
    /// uploads wait for free capacity during that time.
    #[arg(default_value = "8")]
    pub concurrency: NonZeroUsize,

    /// Total size in bytes of admitted uploads. Uploads that exceed the remaining budget wait.
    /// An upload larger than the whole budget can run alone once nothing else is in flight.
    #[arg(default_value = "8589934592")]
    pub bytes_budget: NonZeroU64,

    /// Seconds granted worker uploads can go without data before they lose their permit and
    /// their workers receive a request to retry.
    #[arg(default_value = "300")]
    pub lease_idle_secs: NonZeroU64,

    /// Seconds that uploads of NARs to the upload endpoint of the cache and new store paths sent
    /// over SSH with `nix copy` wait for a permit. The upload endpoint can return a 503 answer with
    /// `Retry-After` once the wait time is over. `nix copy` can fail with "upload capacity
    /// exhausted, retry later". Store paths in the cache need no permit.
    #[arg(default_value = "30")]
    pub rest_wait_secs: NonZeroU64,
}
