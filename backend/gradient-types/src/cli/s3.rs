/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct S3Args {
    /// S3 bucket name. NARs are stored in S3 instead of local disk with a bucket set.
    #[arg(long = "s3-bucket", env = "GRADIENT_S3_BUCKET")]
    pub bucket: Option<String>,
    /// AWS region for the S3 bucket.
    #[arg(
        long = "s3-region",
        env = "GRADIENT_S3_REGION",
        default_value = "us-east-1"
    )]
    pub region: String,
    /// Custom S3-compatible endpoint URL (MinIO, Cloudflare R2, …).
    #[arg(long = "s3-endpoint", env = "GRADIENT_S3_ENDPOINT")]
    pub endpoint: Option<String>,
    /// AWS access key ID. An absent ID is falling back to instance credentials.
    #[arg(long = "s3-access-key-id", env = "GRADIENT_S3_ACCESS_KEY_ID")]
    pub access_key_id: Option<String>,
    /// File containing the AWS secret access key.
    #[arg(
        long = "s3-secret-access-key-file",
        env = "GRADIENT_S3_SECRET_ACCESS_KEY_FILE"
    )]
    pub secret_access_key_file: Option<String>,
    /// Key prefix within the S3 bucket (e.g. "gradient/").
    #[arg(long = "s3-prefix", env = "GRADIENT_S3_PREFIX", default_value = "")]
    pub prefix: String,
    /// Use virtual-hosted-style requests (`https://<bucket>.<endpoint>/key`) with a custom
    /// endpoint. The `false` default is keeping URLs path-style
    /// (`https://<endpoint>/<bucket>/key`). MinIO, Garage and most self-hosted S3-compatible
    /// backends are requiring path style. Set it to `true` for providers needing virtual-hosted
    /// addressing, like Cloudflare R2 with a custom domain or some Backblaze B2 setups. AWS direct
    /// without an endpoint is ignoring this flag.
    #[arg(
        long = "s3-virtual-hosted-style",
        env = "GRADIENT_S3_VIRTUAL_HOSTED_STYLE",
        default_value_t = false
    )]
    pub virtual_hosted_style: bool,
    /// Seconds a single S3 response may stall before the request is failed. This inactivity timer
    /// is resetting on every successful read and is not capping the transfer. A multi-GB NAR is
    /// streaming for as long as it keeps making progress. It is replacing the object-store default
    /// of a flat 30s total request timeout. That default was cancelling any slower download and
    /// burning the whole retry budget on doomed re-runs.
    #[arg(
        long = "s3-read-timeout-secs",
        env = "GRADIENT_S3_READ_TIMEOUT_SECS",
        default_value_t = 60
    )]
    pub read_timeout_secs: u64,
    /// How many times a failed S3 request is retried.
    #[arg(
        long = "s3-max-retries",
        env = "GRADIENT_S3_MAX_RETRIES",
        default_value_t = 3
    )]
    pub max_retries: usize,
    /// Total seconds from the first attempt after which no further S3 retry is started. Keep it
    /// above `(max_retries + 1) * read_timeout_secs`. Requests dying on the read timeout are
    /// otherwise never retried because the budget is already spent. Only the error path is
    /// consulting it. It is never interrupting a progressing download. Stay under 5 minutes because
    /// retries are reusing the original credentials and payload.
    #[arg(
        long = "s3-retry-timeout-secs",
        env = "GRADIENT_S3_RETRY_TIMEOUT_SECS",
        default_value_t = 250
    )]
    pub retry_timeout_secs: u64,
}

impl Default for S3Args {
    fn default() -> Self {
        Self {
            bucket: None,
            region: "us-east-1".into(),
            endpoint: None,
            access_key_id: None,
            secret_access_key_file: None,
            prefix: String::new(),
            virtual_hosted_style: false,
            read_timeout_secs: 60,
            max_retries: 3,
            retry_timeout_secs: 250,
        }
    }
}
