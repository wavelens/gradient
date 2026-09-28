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
use super::cli::{
    BuildArgs, CacheArgs, DatabaseArgs, EvalArgs, GcArgs, HttpArgs, LogArgs, MetricsArgs, NarArgs,
    PermissionsArgs, ProtoArgs, PullRequestsArgs, RegistrationArgs, SchedulerArgs, SecretsArgs,
    SentryArgs, ServerArgs, StateArgs, UploadArgs,
};
use ipnet::IpNet;

/// OIDC configuration - only present when `oidc.enable` is true and all
/// required fields are configured.
#[derive(Debug, Clone)]
pub struct OidcConfig {
    pub client_id: String,
    pub client_secret_file: String,
    /// Optional space-separated scope list; defaults to `"openid email profile"`.
    pub scopes: Option<String>,
    pub discovery_url: String,
    /// Whether OIDC is the only allowed login method.
    pub required: bool,
}

/// SCIM provisioning configuration - only present when `scim.enable` is true
/// and a token file is configured.
#[derive(Debug, Clone)]
pub struct ScimConfig {
    pub token_file: String,
    /// When true, SCIM `DELETE /Users/{id}` hard-deletes (cascade) instead of soft-disabling.
    pub hard_delete: bool,
}

/// Email/SMTP configuration - only present when `email.enable` is true and
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
    /// Whether new users must verify their email before logging in.
    pub require_verification: bool,
}

/// GitHub App configuration - only present when all three fields are set.
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

/// Metrics endpoint configuration - only present when `metrics_token_file`
/// is set and the file contains a non-empty token. The token is loaded once
/// at startup; rotation requires a server restart.
#[derive(Debug, Clone)]
pub struct MetricsConfig {
    /// Bearer token, loaded from `metrics_token_file` at startup.
    pub token: String,
}

/// Parsed network allowlists derived from `HttpArgs`. Both lists are
/// validated once at startup; malformed CIDR entries abort the process.
#[derive(Debug, Clone, Default)]
pub struct NetworkConfig {
    pub trusted_proxies: Vec<IpNet>,
    pub local_ips: Vec<IpNet>,
}

/// S3 / object-storage configuration - only present when `s3.bucket` is set.
#[derive(Debug, Clone)]
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    /// Custom endpoint URL for S3-compatible stores (MinIO, Cloudflare R2, …).
    pub endpoint: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key_file: Option<String>,
    pub prefix: String,
    /// Use virtual-hosted-style addressing when a custom endpoint is set.
    /// `false` (default) requests path-style URLs - MinIO/Garage/most
    /// self-hosted backends require this. No effect on AWS direct.
    pub virtual_hosted_style: bool,
    /// Per-response inactivity timeout; see [`super::cli::S3Args`] for why this
    /// replaces a total request timeout.
    pub read_timeout: std::time::Duration,
    /// Retries for a failed request.
    pub max_retries: usize,
    /// Total budget for those retries, counted from the first attempt.
    pub retry_timeout: std::time::Duration,
}

impl Cli {
    /// Returns the typed OIDC config when OIDC is enabled and fully configured.
    pub fn oidc_config(&self) -> Option<OidcConfig> {
        if !self.oidc.enable {
            return None;
        }
        Some(OidcConfig {
            client_id: self.oidc.client_id.clone()?,
            client_secret_file: self.oidc.client_secret_file.clone()?,
            scopes: self.oidc.scopes.clone(),
            discovery_url: self.oidc.discovery_url.clone()?,
            required: self.oidc.required,
        })
    }

    /// Returns the typed SCIM config when SCIM is enabled and a token file is set.
    pub fn scim_config(&self) -> Option<ScimConfig> {
        if !self.scim.enable {
            return None;
        }
        Some(ScimConfig {
            token_file: self.scim.token_file.clone()?,
            hard_delete: self.scim.hard_delete,
        })
    }

    /// Returns the typed email config when email is enabled and fully configured.
    pub fn email_config(&self) -> Option<EmailConfig> {
        if !self.email.enable {
            return None;
        }
        Some(EmailConfig {
            smtp_host: self.email.smtp_host.clone()?,
            smtp_port: self.email.smtp_port,
            smtp_username: self.email.smtp_username.clone()?,
            smtp_password_file: self.email.smtp_password_file.clone()?,
            from_address: self.email.from_address.clone()?,
            from_name: self.email.from_name.clone(),
            enable_tls: self.email.smtp_use_tls,
            require_verification: self.email.require_verification,
        })
    }

    /// Returns the typed GitHub App config when all three GitHub App fields
    /// are configured.
    pub fn github_app_config(&self) -> Option<GitHubAppConfig> {
        Some(GitHubAppConfig {
            app_id: self.github_app.id?,
            private_key_file: self.github_app.private_key_file.clone()?,
            webhook_secret_file: self.github_app.webhook_secret_file.clone()?,
        })
    }

    /// Returns the typed S3 config when an S3 bucket is configured.
    pub fn s3_config(&self) -> Option<S3Config> {
        self.s3.bucket.as_ref().map(|bucket| S3Config {
            bucket: bucket.clone(),
            region: self.s3.region.clone(),
            endpoint: self.s3.endpoint.clone(),
            access_key_id: self.s3.access_key_id.clone(),
            secret_access_key_file: self.s3.secret_access_key_file.clone(),
            prefix: self.s3.prefix.clone(),
            virtual_hosted_style: self.s3.virtual_hosted_style,
            read_timeout: std::time::Duration::from_secs(self.s3.read_timeout_secs),
            max_retries: self.s3.max_retries,
            retry_timeout: std::time::Duration::from_secs(self.s3.retry_timeout_secs),
        })
    }

    /// Returns the resolved network config, naming the offending env var if
    /// either CIDR list fails to parse.
    pub fn network_config(&self) -> Result<NetworkConfig, ConfigError> {
        Ok(NetworkConfig {
            trusted_proxies: super::cli::parse_cidr_list(&self.http.trusted_proxies)
                .map_err(ConfigError::TrustedProxies)?,
            local_ips: super::cli::parse_cidr_list(&self.http.local_ips)
                .map_err(ConfigError::LocalIps)?,
        })
    }

    /// Returns the typed metrics config when a token file path is configured
    /// and the file contains a non-empty token after trimming.
    pub fn metrics_config(&self) -> Option<MetricsConfig> {
        let path = self.metrics.token_file.as_ref()?;
        let raw = std::fs::read_to_string(path).ok()?;
        let token = raw.trim().to_string();
        if token.is_empty() {
            return None;
        }
        Some(MetricsConfig { token })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("GRADIENT_HTTP_TRUSTED_PROXIES: {0}")]
    TrustedProxies(#[source] super::cli::CidrParseError),
    #[error("GRADIENT_HTTP_LOCAL_IPS: {0}")]
    LocalIps(#[source] super::cli::CidrParseError),
}

/// Resolved runtime configuration carried by `AppState`.
///
/// Built once at startup from a parsed [`Cli`]. Handlers depend on the slice
/// they need (`state.config.<group>.<field>`) instead of the whole
/// parser DTO.
///
/// Optional features (`oidc`, `email`, `s3`, `github_app`) are `None` when
/// disabled or incompletely configured - the `Some` variant guarantees the
/// feature is fully usable.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub log: LogArgs,
    pub server: ServerArgs,
    pub secrets: SecretsArgs,
    pub state: StateArgs,
    pub permissions: PermissionsArgs,
    pub registration: RegistrationArgs,
    pub sentry: SentryArgs,
    pub pull_requests: PullRequestsArgs,
    pub database: DatabaseArgs,
    pub http: HttpArgs,
    pub proto: ProtoArgs,
    pub upload: UploadArgs,
    pub nar: NarArgs,
    pub cache: CacheArgs,
    pub gc: GcArgs,
    pub eval: EvalArgs,
    pub build: BuildArgs,
    pub scheduler: SchedulerArgs,
    pub oidc: Option<OidcConfig>,
    pub scim: Option<ScimConfig>,
    pub email: Option<EmailConfig>,
    pub s3: Option<S3Config>,
    pub github_app: Option<GitHubAppConfig>,
    pub metrics: Option<MetricsConfig>,
    /// Always-present metrics pipeline settings (rollup interval, retention,
    /// OTLP, sampling). Distinct from `metrics`, which gates the scrape token.
    pub metrics_args: MetricsArgs,
    pub network: NetworkConfig,
}

impl RuntimeConfig {
    /// Resolve a parsed [`Cli`] into a runtime configuration. Optional
    /// features collapse to `None` exactly when their accessor methods do.
    pub fn from_cli(cli: &Cli) -> Result<Self, ConfigError> {
        Ok(Self {
            log: cli.log.clone(),
            server: cli.server.clone(),
            secrets: cli.secrets.clone(),
            state: cli.state.clone(),
            permissions: cli.permissions.clone(),
            registration: cli.registration.clone(),
            sentry: cli.sentry.clone(),
            pull_requests: cli.pull_requests.clone(),
            database: cli.database.clone(),
            http: cli.http.clone(),
            proto: cli.proto.clone(),
            upload: cli.upload.clone(),
            nar: cli.nar.clone(),
            cache: cli.cache.clone(),
            gc: cli.gc.clone(),
            eval: cli.eval.clone(),
            build: cli.build.clone(),
            scheduler: cli.scheduler.clone(),
            oidc: cli.oidc_config(),
            scim: cli.scim_config(),
            email: cli.email_config(),
            s3: cli.s3_config(),
            github_app: cli.github_app_config(),
            metrics: cli.metrics_config(),
            metrics_args: cli.metrics.clone(),
            network: cli.network_config()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_cli() -> Cli {
        use crate::cli::*;
        Cli {
            log: LogArgs {
                level_default: "error".into(),
                ..Default::default()
            },
            server: ServerArgs {
                serve_url: "http://127.0.0.1:3000".into(),
                use_tls: false,
                base_dir: "/tmp/gradient-test".into(),
                ..Default::default()
            },
            secrets: SecretsArgs {
                crypt_file: "test-secret".into(),
                jwt_file: "test-jwt".into(),
            },
            registration: RegistrationArgs { enable: false },
            proto: ProtoArgs {
                max_connections: 16,
                discoverable: false,
                ..Default::default()
            },
            email: EmailArgs {
                from_name: "Gradient Test".into(),
                smtp_use_tls: false,
                ..Default::default()
            },
            ..Default::default()
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
        cli.oidc.enable = true;
        // oidc_client_id, oidc_client_secret_file, oidc_discovery_url all None
        assert!(cli.oidc_config().is_none());
    }

    #[test]
    fn oidc_config_fully_configured_returns_some() {
        let mut cli = base_cli();
        cli.oidc.enable = true;
        cli.oidc.client_id = Some("client-id".into());
        cli.oidc.client_secret_file = Some("/run/secrets/oidc".into());
        cli.oidc.discovery_url = Some("https://idp.example.com".into());
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
        cli.email.enable = true;
        // email_smtp_host is None
        assert!(cli.email_config().is_none());
    }

    #[test]
    fn email_config_fully_configured_returns_some() {
        let mut cli = base_cli();
        cli.email.enable = true;
        cli.email.smtp_host = Some("smtp.example.com".into());
        cli.email.smtp_username = Some("user".into());
        cli.email.smtp_password_file = Some("/run/secrets/smtp".into());
        cli.email.from_address = Some("gradient@example.com".into());
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
        cli.github_app.id = Some(42);
        // private_key_file and webhook_secret_file still None
        assert!(cli.github_app_config().is_none());
    }

    #[test]
    fn github_app_config_fully_configured_returns_some() {
        let mut cli = base_cli();
        cli.github_app.id = Some(12345);
        cli.github_app.private_key_file = Some("/run/secrets/github-app.pem".into());
        cli.github_app.webhook_secret_file = Some("/run/secrets/github-webhook".into());
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
        cli.s3.bucket = Some("my-bucket".into());
        let config = cli.s3_config().expect("should return Some");
        assert_eq!(config.bucket, "my-bucket");
        assert_eq!(config.region, "us-east-1");
        assert!(config.endpoint.is_none());
    }

    #[test]
    fn runtime_config_from_default_cli_has_no_optional_features() {
        let runtime = RuntimeConfig::from_cli(&base_cli()).expect("valid");
        assert!(runtime.oidc.is_none());
        assert!(runtime.email.is_none());
        assert!(runtime.s3.is_none());
        assert!(runtime.github_app.is_none());
        assert_eq!(runtime.server.base_dir, "/tmp/gradient-test");
    }

    #[test]
    fn runtime_config_populates_optional_features_when_configured() {
        let mut cli = base_cli();
        cli.oidc.enable = true;
        cli.oidc.required = true;
        cli.oidc.client_id = Some("cid".into());
        cli.oidc.client_secret_file = Some("/run/secrets/oidc".into());
        cli.oidc.discovery_url = Some("https://idp.example.com".into());
        cli.email.enable = true;
        cli.email.require_verification = true;
        cli.email.smtp_host = Some("smtp.example.com".into());
        cli.email.smtp_username = Some("u".into());
        cli.email.smtp_password_file = Some("/run/secrets/smtp".into());
        cli.email.from_address = Some("g@example.com".into());
        cli.s3.bucket = Some("bkt".into());
        cli.github_app.id = Some(1);
        cli.github_app.private_key_file = Some("/k".into());
        cli.github_app.webhook_secret_file = Some("/w".into());

        let runtime = RuntimeConfig::from_cli(&cli).expect("valid");
        let oidc = runtime.oidc.expect("oidc populated");
        assert!(oidc.required);
        let email = runtime.email.expect("email populated");
        assert!(email.require_verification);
        assert!(runtime.s3.is_some());
        assert!(runtime.github_app.is_some());
    }

    #[test]
    fn network_config_defaults_parse() {
        let cli = base_cli();
        let cfg = cli.network_config().expect("default CIDR lists parse");
        assert_eq!(cfg.trusted_proxies.len(), 2); // 127.0.0.1/32 + ::1/128
        assert_eq!(cfg.local_ips.len(), 1); // 10.0.0.0/8
    }

    #[test]
    fn network_config_invalid_trusted_proxies_returns_err() {
        let mut cli = base_cli();
        cli.http.trusted_proxies = "not-a-cidr".into();
        let err = cli.network_config().unwrap_err();
        assert!(err.to_string().contains("GRADIENT_HTTP_TRUSTED_PROXIES"));
    }

    #[test]
    fn network_config_invalid_local_ips_returns_err() {
        let mut cli = base_cli();
        cli.http.local_ips = "10.0.0.0/8, banana".into();
        let err = cli.network_config().unwrap_err();
        assert!(err.to_string().contains("GRADIENT_HTTP_LOCAL_IPS"));
        assert!(err.to_string().contains("banana"));
    }

    #[test]
    fn metrics_config_unset_returns_none() {
        let cli = base_cli();
        assert!(cli.metrics_config().is_none());
    }

    #[test]
    fn metrics_config_empty_file_returns_none() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        write!(tmp, "   \n  ").expect("write");
        let path = tmp.path().to_string_lossy().into_owned();

        let mut cli = base_cli();
        cli.metrics.token_file = Some(path);
        assert!(cli.metrics_config().is_none());
    }

    #[test]
    fn metrics_config_loaded_from_file() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        writeln!(tmp, "  s3cret-token").expect("write");
        let path = tmp.path().to_string_lossy().into_owned();

        let mut cli = base_cli();
        cli.metrics.token_file = Some(path);

        let cfg = cli.metrics_config().expect("Some");
        assert_eq!(cfg.token, "s3cret-token");
    }

    #[test]
    fn runtime_config_metrics_propagates() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        write!(tmp, "tok").expect("write");
        let path = tmp.path().to_string_lossy().into_owned();

        let mut cli = base_cli();
        cli.metrics.token_file = Some(path);
        let runtime = RuntimeConfig::from_cli(&cli).expect("valid");
        assert!(runtime.metrics.is_some());
    }

    #[test]
    fn scim_config_disabled_returns_none() {
        let cli = base_cli();
        assert!(cli.scim_config().is_none());
    }

    #[test]
    fn scim_config_enabled_missing_token_returns_none() {
        let mut cli = base_cli();
        cli.scim.enable = true;
        assert!(cli.scim_config().is_none());
    }

    #[test]
    fn scim_config_fully_configured_returns_some() {
        let mut cli = base_cli();
        cli.scim.enable = true;
        cli.scim.token_file = Some("/run/secrets/scim".into());
        cli.scim.hard_delete = true;
        let cfg = cli.scim_config().expect("should return Some");
        assert_eq!(cfg.token_file, "/run/secrets/scim");
        assert!(cfg.hard_delete);
    }

    #[test]
    fn runtime_config_scim_propagates() {
        let mut cli = base_cli();
        cli.scim.enable = true;
        cli.scim.token_file = Some("/run/secrets/scim".into());
        let runtime = RuntimeConfig::from_cli(&cli).expect("valid");
        assert!(runtime.scim.is_some());
    }
}
