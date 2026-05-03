/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Typed configuration clusters extracted from the flat `Cli` struct.
//!
//! Each struct groups the fields of one feature that is either fully enabled
//! (all fields present) or fully disabled (feature flag is false / bucket not
//! set).  The accessor methods on [`Cli`] return `Option<T>`: `None` means the
//! feature is disabled or misconfigured, `Some(config)` means every required
//! field is present and the feature can be used.

use super::Cli;

/// OIDC configuration — only present when `oidc_enabled` is true and all
/// required fields are configured.
#[derive(Debug, Clone)]
pub struct OidcConfig {
    pub client_id: String,
    pub client_secret_file: String,
    /// Optional space-separated scope list; defaults to `"openid email profile"`.
    pub scopes: Option<String>,
    pub discovery_url: String,
}

/// Email/SMTP configuration — only present when `email_enabled` is true and
/// all required fields are configured.
#[derive(Debug, Clone)]
pub struct EmailConfig {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password_file: String,
    pub from_address: String,
    pub from_name: String,
    pub enable_tls: bool,
}

/// GitHub App configuration — only present when all three fields are set.
///
/// Required together: the App ID and private key are needed to generate
/// short-lived JWTs for API authentication, and the webhook secret is needed
/// to verify incoming payloads. An incomplete configuration is treated as
/// "GitHub App disabled".
#[derive(Debug, Clone)]
pub struct GitHubAppConfig {
    /// Numeric GitHub App ID.
    pub app_id: u64,
    /// Path to the RS256 PEM private key file.
    pub private_key_file: String,
    /// Path to the shared webhook secret file used to verify
    /// `X-Hub-Signature-256` headers.
    pub webhook_secret_file: String,
}

/// S3 / object-storage configuration — only present when `s3_bucket` is set.
#[derive(Debug, Clone)]
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    /// Custom endpoint URL for S3-compatible stores (MinIO, Cloudflare R2, …).
    pub endpoint: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key_file: Option<String>,
    pub prefix: String,
}

impl Cli {
    /// Returns the typed OIDC config when OIDC is enabled and fully configured.
    pub fn oidc_config(&self) -> Option<OidcConfig> {
        if !self.oidc.oidc_enabled {
            return None;
        }
        Some(OidcConfig {
            client_id: self.oidc.oidc_client_id.clone()?,
            client_secret_file: self.oidc.oidc_client_secret_file.clone()?,
            scopes: self.oidc.oidc_scopes.clone(),
            discovery_url: self.oidc.oidc_discovery_url.clone()?,
        })
    }

    /// Returns the typed email config when email is enabled and fully configured.
    pub fn email_config(&self) -> Option<EmailConfig> {
        if !self.email.email_enabled {
            return None;
        }
        Some(EmailConfig {
            smtp_host: self.email.email_smtp_host.clone()?,
            smtp_port: self.email.email_smtp_port,
            smtp_username: self.email.email_smtp_username.clone()?,
            smtp_password_file: self.email.email_smtp_password_file.clone()?,
            from_address: self.email.email_from_address.clone()?,
            from_name: self.email.email_from_name.clone(),
            enable_tls: self.email.email_enable_tls,
        })
    }

    /// Returns the typed GitHub App config when all three GitHub App fields
    /// are configured.
    pub fn github_app_config(&self) -> Option<GitHubAppConfig> {
        Some(GitHubAppConfig {
            app_id: self.github_app.github_app_id?,
            private_key_file: self.github_app.github_app_private_key_file.clone()?,
            webhook_secret_file: self.github_app.github_app_webhook_secret_file.clone()?,
        })
    }

    /// Returns the typed S3 config when an S3 bucket is configured.
    pub fn s3_config(&self) -> Option<S3Config> {
        self.s3.s3_bucket.as_ref().map(|bucket| S3Config {
            bucket: bucket.clone(),
            region: self.s3.s3_region.clone(),
            endpoint: self.s3.s3_endpoint.clone(),
            access_key_id: self.s3.s3_access_key_id.clone(),
            secret_access_key_file: self.s3.s3_secret_access_key_file.clone(),
            prefix: self.s3.s3_prefix.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_cli() -> Cli {
        use crate::types::cli::*;
        Cli {
            logging: LoggingArgs {
                log_level: "error".into(),
                ..Default::default()
            },
            server: ServerArgs {
                serve_url: "http://127.0.0.1:3000".into(),
                use_tls: false,
                ..Default::default()
            },
            database: DatabaseArgs::default(),
            eval: EvalArgs {
                max_concurrent_evaluations: 2,
                max_concurrent_builds: 10,
                evaluation_timeout: 5,
                eval_workers: 1,
                max_evaluations_per_worker: 0,
            },
            storage: StorageArgs {
                base_path: "/tmp/gradient-test".into(),
                keep_evaluations: 30,
                ..Default::default()
            },
            secrets: SecretsArgs {
                crypt_secret_file: "test-secret".into(),
                jwt_secret_file: "test-jwt".into(),
            },
            limits: LimitsArgs::default(),
            registration: RegistrationArgs {
                enable_registration: false,
                report_errors: false,
            },
            proto: ProtoArgs {
                max_proto_connections: 16,
                discoverable: false,
                ..Default::default()
            },
            oidc: OidcArgs::default(),
            email: EmailArgs {
                email_from_name: "Gradient Test".into(),
                email_enable_tls: false,
                ..Default::default()
            },
            s3: S3Args::default(),
            github_app: GitHubAppArgs::default(),
        }
    }

    #[test]
    fn oidc_config_disabled_returns_none() {
        let cli = base_cli();
        assert!(cli.oidc_config().is_none());
    }

    #[test]
    fn oidc_config_enabled_missing_fields_returns_none() {
        let mut cli = base_cli();
        cli.oidc.oidc_enabled = true;
        // oidc_client_id, oidc_client_secret_file, oidc_discovery_url all None
        assert!(cli.oidc_config().is_none());
    }

    #[test]
    fn oidc_config_fully_configured_returns_some() {
        let mut cli = base_cli();
        cli.oidc.oidc_enabled = true;
        cli.oidc.oidc_client_id = Some("client-id".into());
        cli.oidc.oidc_client_secret_file = Some("/run/secrets/oidc".into());
        cli.oidc.oidc_discovery_url = Some("https://idp.example.com".into());
        let config = cli.oidc_config().expect("should return Some");
        assert_eq!(config.client_id, "client-id");
        assert!(config.scopes.is_none());
    }

    #[test]
    fn email_config_disabled_returns_none() {
        let cli = base_cli();
        assert!(cli.email_config().is_none());
    }

    #[test]
    fn email_config_enabled_missing_host_returns_none() {
        let mut cli = base_cli();
        cli.email.email_enabled = true;
        // email_smtp_host is None
        assert!(cli.email_config().is_none());
    }

    #[test]
    fn email_config_fully_configured_returns_some() {
        let mut cli = base_cli();
        cli.email.email_enabled = true;
        cli.email.email_smtp_host = Some("smtp.example.com".into());
        cli.email.email_smtp_username = Some("user".into());
        cli.email.email_smtp_password_file = Some("/run/secrets/smtp".into());
        cli.email.email_from_address = Some("gradient@example.com".into());
        let config = cli.email_config().expect("should return Some");
        assert_eq!(config.smtp_host, "smtp.example.com");
        assert_eq!(config.smtp_port, 587);
    }

    #[test]
    fn github_app_config_all_missing_returns_none() {
        let cli = base_cli();
        assert!(cli.github_app_config().is_none());
    }

    #[test]
    fn github_app_config_partial_returns_none() {
        let mut cli = base_cli();
        cli.github_app.github_app_id = Some(42);
        // private_key_file and webhook_secret_file still None
        assert!(cli.github_app_config().is_none());
    }

    #[test]
    fn github_app_config_fully_configured_returns_some() {
        let mut cli = base_cli();
        cli.github_app.github_app_id = Some(12345);
        cli.github_app.github_app_private_key_file = Some("/run/secrets/github-app.pem".into());
        cli.github_app.github_app_webhook_secret_file = Some("/run/secrets/github-webhook".into());
        let config = cli.github_app_config().expect("should return Some");
        assert_eq!(config.app_id, 12345);
        assert_eq!(config.private_key_file, "/run/secrets/github-app.pem");
        assert_eq!(config.webhook_secret_file, "/run/secrets/github-webhook");
    }

    #[test]
    fn s3_config_no_bucket_returns_none() {
        let cli = base_cli();
        assert!(cli.s3_config().is_none());
    }

    #[test]
    fn s3_config_with_bucket_returns_some() {
        let mut cli = base_cli();
        cli.s3.s3_bucket = Some("my-bucket".into());
        let config = cli.s3_config().expect("should return Some");
        assert_eq!(config.bucket, "my-bucket");
        assert_eq!(config.region, "us-east-1");
        assert!(config.endpoint.is_none());
    }
}
