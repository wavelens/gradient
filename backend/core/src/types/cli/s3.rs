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
        }
    }
}
