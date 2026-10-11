/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("s3")]
#[derive(Args, Debug, Clone)]
pub struct S3Args {
    /// Name of the S3 bucket. The bucket must not have versioning, object lock or replication
    /// enabled. Object overwrites happen in place, and old versions stay in the bucket.
    /// Versioned buckets thus keep an unreclaimable copy per upload.
    pub bucket: Option<String>,
    /// Region of the S3 bucket.
    #[arg(default_value = "us-east-1")]
    pub region: String,
    /// Endpoint of an S3-compatible service such as MinIO or Cloudflare R2. Set it to `null` to
    /// use AWS.
    pub endpoint: Option<String>,
    /// AWS access key ID. Set it to `null` to use instance credentials or the environment.
    pub access_key_id: Option<String>,
    /// File containing the AWS secret access key.
    pub secret_access_key_file: Option<String>,
    /// Key prefix inside the bucket, such as `gradient/`. Leave it empty to store at the bucket
    /// root.
    #[arg(default_value = "")]
    pub prefix: String,
    /// Whether to address a custom {option}`services.gradient.s3.endpoint` virtual-hosted style
    /// (`https://<bucket>.<endpoint>/key`) instead of path style
    /// (`https://<endpoint>/<bucket>/key`). MinIO, Garage and most self-hosted services need
    /// path style. It has no effect without a custom endpoint.
    #[arg(default_value_t = false)]
    pub virtual_hosted_style: bool,
    /// Seconds an S3 response can stall before failing the request. Received chunks reset the
    /// timer, and large NARs keep streaming as long as they make progress.
    #[arg(default_value_t = 60)]
    pub read_timeout_secs: u64,
    /// Retries of a failed S3 request.
    #[arg(default_value_t = 3)]
    pub max_retries: usize,
    /// Seconds after the first attempt past which no S3 retries start. Keep it above
    /// `(maxRetries + 1) * readTimeoutSecs` to still retry requests that fail on the read
    /// timeout. Keep it below 5 minutes as well, since retries reuse the original credentials.
    #[arg(default_value_t = 250)]
    pub retry_timeout_secs: u64,
}
