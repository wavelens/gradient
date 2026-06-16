/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct S3Args {
    /// S3 bucket name. When set, NARs are stored in S3 instead of local disk.
    #[arg(long, env = "GRADIENT_S3_BUCKET")]
    pub s3_bucket: Option<String>,
    /// AWS region for the S3 bucket.
    #[arg(long, env = "GRADIENT_S3_REGION", default_value = "us-east-1")]
    pub s3_region: String,
    /// Custom S3-compatible endpoint URL (MinIO, Cloudflare R2, …).
    #[arg(long, env = "GRADIENT_S3_ENDPOINT")]
    pub s3_endpoint: Option<String>,
    /// AWS access key ID. Falls back to instance credentials when absent.
    #[arg(long, env = "GRADIENT_S3_ACCESS_KEY_ID")]
    pub s3_access_key_id: Option<String>,
    /// File containing the AWS secret access key.
    #[arg(long, env = "GRADIENT_S3_SECRET_ACCESS_KEY_FILE")]
    pub s3_secret_access_key_file: Option<String>,
    /// Key prefix within the S3 bucket (e.g. "gradient/").
    #[arg(long, env = "GRADIENT_S3_PREFIX", default_value = "")]
    pub s3_prefix: String,
    /// Use virtual-hosted-style requests (`https://<bucket>.<endpoint>/key`)
    /// when a custom endpoint is set. Defaults to `false` so the URL is
    /// path-style (`https://<endpoint>/<bucket>/key`) - required by MinIO,
    /// Garage, and most self-hosted S3-compatible backends. Set to `true`
    /// for providers that demand virtual-hosted addressing (Cloudflare R2
    /// with a custom domain, some Backblaze B2 setups). Has no effect on
    /// AWS direct (no endpoint set).
    #[arg(
        long,
        env = "GRADIENT_S3_VIRTUAL_HOSTED_STYLE",
        default_value_t = false
    )]
    pub s3_virtual_hosted_style: bool,
}

impl Default for S3Args {
    fn default() -> Self {
        Self {
            s3_bucket: None,
            s3_region: "us-east-1".into(),
            s3_endpoint: None,
            s3_access_key_id: None,
            s3_secret_access_key_file: None,
            s3_prefix: String::new(),
            s3_virtual_hosted_style: false,
        }
    }
}
