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
    ///
    /// Returns `None` when `oidc_enabled` is `false` or any required field
    /// (`oidc_client_id`, `oidc_client_secret_file`, `oidc_discovery_url`) is
    /// absent.
    pub fn oidc_config(&self) -> Option<OidcConfig> {
        if !self.oidc_enabled {
            return None;
        }
        Some(OidcConfig {
            client_id: self.oidc_client_id.clone()?,
            client_secret_file: self.oidc_client_secret_file.clone()?,
            scopes: self.oidc_scopes.clone(),
            discovery_url: self.oidc_discovery_url.clone()?,
        })
    }

    /// Returns the typed email config when email is enabled and fully configured.
    ///
    /// Returns `None` when `email_enabled` is `false` or any required field
    /// (`email_smtp_host`, `email_smtp_username`, `email_smtp_password_file`,
    /// `email_from_address`) is absent.
    pub fn email_config(&self) -> Option<EmailConfig> {
        if !self.email_enabled {
            return None;
        }
        Some(EmailConfig {
            smtp_host: self.email_smtp_host.clone()?,
            smtp_port: self.email_smtp_port,
            smtp_username: self.email_smtp_username.clone()?,
            smtp_password_file: self.email_smtp_password_file.clone()?,
            from_address: self.email_from_address.clone()?,
            from_name: self.email_from_name.clone(),
            enable_tls: self.email_enable_tls,
        })
    }

    /// Returns the typed GitHub App config when all three GitHub App fields
    /// are configured.
    ///
    /// Returns `None` when any of `github_app_id`, `github_app_private_key_file`,
    /// or `github_app_webhook_secret_file` is absent.
    pub fn github_app_config(&self) -> Option<GitHubAppConfig> {
        Some(GitHubAppConfig {
            app_id: self.github_app_id?,
            private_key_file: self.github_app_private_key_file.clone()?,
            webhook_secret_file: self.github_app_webhook_secret_file.clone()?,
        })
    }

    /// Returns the typed S3 config when an S3 bucket is configured.
    ///
    /// Returns `None` when `s3_bucket` is not set (local storage is used).
    pub fn s3_config(&self) -> Option<S3Config> {
        self.s3_bucket.as_ref().map(|bucket| S3Config {
            bucket: bucket.clone(),
            region: self.s3_region.clone(),
            endpoint: self.s3_endpoint.clone(),
            access_key_id: self.s3_access_key_id.clone(),
            secret_access_key_file: self.s3_secret_access_key_file.clone(),
            prefix: self.s3_prefix.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_cli() -> Cli {
        Cli {
            log_level: "error".into(),
            builder_log_level: None,
            cache_log_level: None,
            web_log_level: None,
            proto_log_level: None,
            ip: "127.0.0.1".into(),
            port: 3000,
            serve_url: "http://127.0.0.1:3000".into(),
            database_url: None,
            database_url_file: None,
            max_concurrent_evaluations: 2,
            max_concurrent_builds: 10,
            evaluation_timeout: 5,
            store_path: None,
            base_path: "/tmp/gradient-test".into(),
            enable_registration: false,
            oidc_enabled: false,
            oidc_required: false,
            oidc_client_id: None,
            oidc_client_secret_file: None,
            oidc_scopes: None,
            oidc_discovery_url: None,
            crypt_secret_file: "test-secret".into(),
            jwt_secret_file: "test-jwt".into(),
            serve_cache: false,
            report_errors: false,
            email_enabled: false,
            email_require_verification: false,
            email_smtp_host: None,
            email_smtp_port: 587,
            email_smtp_username: None,
            email_smtp_password_file: None,
            email_from_address: None,
            email_from_name: "Gradient Test".into(),
            email_enable_tls: false,
            state_file: None,
            delete_state: true,
            keep_evaluations: 30,
            keep_orphan_derivations_hours: 24,
            max_nixdaemon_connections: 2,
            eval_workers: 1,
            max_evaluations_per_worker: 0,
            nar_ttl_hours: 0,
            s3_bucket: None,
            s3_region: "us-east-1".into(),
            s3_endpoint: None,
            s3_access_key_id: None,
            s3_secret_access_key_file: None,
            s3_prefix: String::new(),
            frontend_url: "http://127.0.0.1:8000".into(),
            github_app_id: None,
            github_app_private_key_file: None,
            github_app_webhook_secret_file: None,
            quic: false,
            max_proto_connections: 16,
            discoverable: false,
            federate_proto: false,
            global_stats_public: false,
            use_tls: false,
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
        cli.oidc_enabled = true;
        // oidc_client_id, oidc_client_secret_file, oidc_discovery_url all None
        assert!(cli.oidc_config().is_none());
    }

    #[test]
    fn oidc_config_fully_configured_returns_some() {
        let mut cli = base_cli();
        cli.oidc_enabled = true;
        cli.oidc_client_id = Some("client-id".into());
        cli.oidc_client_secret_file = Some("/run/secrets/oidc".into());
        cli.oidc_discovery_url = Some("https://idp.example.com".into());
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
        cli.email_enabled = true;
        // email_smtp_host is None
        assert!(cli.email_config().is_none());
    }

    #[test]
    fn email_config_fully_configured_returns_some() {
        let mut cli = base_cli();
        cli.email_enabled = true;
        cli.email_smtp_host = Some("smtp.example.com".into());
        cli.email_smtp_username = Some("user".into());
        cli.email_smtp_password_file = Some("/run/secrets/smtp".into());
        cli.email_from_address = Some("gradient@example.com".into());
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
        cli.github_app_id = Some(42);
        // private_key_file and webhook_secret_file still None
        assert!(cli.github_app_config().is_none());
    }

    #[test]
    fn github_app_config_fully_configured_returns_some() {
        let mut cli = base_cli();
        cli.github_app_id = Some(12345);
        cli.github_app_private_key_file = Some("/run/secrets/github-app.pem".into());
        cli.github_app_webhook_secret_file = Some("/run/secrets/github-webhook".into());
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
        cli.s3_bucket = Some("my-bucket".into());
        let config = cli.s3_config().expect("should return Some");
        assert_eq!(config.bucket, "my-bucket");
        assert_eq!(config.region, "us-east-1");
        assert!(config.endpoint.is_none());
    }
}
