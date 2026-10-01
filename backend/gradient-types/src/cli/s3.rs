/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct S3Args {
    /// S3 bucket name. When set, NARs are stored in S3 instead of local disk.
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
    /// AWS access key ID. Falls back to instance credentials when absent.
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
    /// Use virtual-hosted-style requests (`https://<bucket>.<endpoint>/key`)
    /// when a custom endpoint is set. Defaults to `false` so the URL is
    /// path-style (`https://<endpoint>/<bucket>/key`) - required by MinIO,
    /// Garage, and most self-hosted S3-compatible backends. Set to `true`
    /// for providers that need virtual-hosted addressing (Cloudflare R2
    /// with a custom domain, some Backblaze B2 setups). Has no effect on
    /// AWS direct (no endpoint set).
    #[arg(
        long = "s3-virtual-hosted-style",
        env = "GRADIENT_S3_VIRTUAL_HOSTED_STYLE",
        default_value_t = false
    )]
    pub virtual_hosted_style: bool,
    /// Seconds a single S3 response may stall before the request is failed.
    /// This is an inactivity timer, reset by every successful read - not a cap
    /// on the transfer, so a multi-GB NAR streams for as long as it keeps making
    /// progress. Replaces the object-store default of a flat 30s *total* request
    /// timeout, which cancelled any download slower than that and then burned
    /// the whole retry budget re-running a request doomed to be cancelled again.
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
    /// Total seconds from the first attempt after which no further S3 retry is
    /// started. Keep it above `(max_retries + 1) * read_timeout_secs` or
    /// requests that die on the read timeout are never retried - the budget is
    /// already spent by the time the first attempt fails. Only consulted on the
    /// error path, so it never interrupts a download that is still progressing.
    /// Stay under 5 minutes: retries reuse the original credentials and payload.
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
