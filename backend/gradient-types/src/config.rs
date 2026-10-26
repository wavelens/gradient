/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::Cli;
use super::cli::{
    BuildArgs, CacheArgs, DatabaseArgs, EvalArgs, GcArgs, GradientCiArgs, HttpArgs, LogArgs,
    MetricsArgs, NarArgs, PermissionsArgs, ProtoArgs, PullRequestsArgs, RegistrationArgs,
    RunnerArgs, SchedulerArgs, SecretsArgs, SentryArgs, ServerArgs, SshArgs, StateArgs, UploadArgs,
};
use ipnet::IpNet;

#[derive(Debug, Clone)]
pub struct OidcConfig {
    pub client_id: String,
    pub client_secret_file: String,
    pub scopes: Vec<String>,
    pub discovery_url: String,
    pub required: bool,
}

#[derive(Debug, Clone)]
pub struct ScimConfig {
    pub token_file: String,
    pub hard_delete: bool,
}

#[derive(Debug, Clone)]
pub struct EmailConfig {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password_file: String,
    pub from_address: String,
    pub from_name: String,
    pub enable_tls: bool,
    pub require_verification: bool,
}

#[derive(Debug, Clone)]
pub struct GitHubAppConfig {
    pub app_id: u64,
    pub private_key_file: String,
    pub webhook_secret_file: String,
}

#[derive(Debug, Clone)]
pub struct MetricsConfig {
    pub token: String,
}

#[derive(Debug, Clone, Default)]
pub struct NetworkConfig {
    pub trusted_proxies: Vec<IpNet>,
    pub local_ips: Vec<IpNet>,
}

#[derive(Debug, Clone)]
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key_file: Option<String>,
    pub prefix: String,
    /// The `false` default is requesting path-style URLs. MinIO, Garage and most self-hosted
    /// backends are requiring path-style. AWS direct is ignoring this flag.
    pub virtual_hosted_style: bool,
    pub read_timeout: std::time::Duration,
    pub max_retries: usize,
    pub retry_timeout: std::time::Duration,
}

impl Cli {
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

    pub fn scim_config(&self) -> Option<ScimConfig> {
        if !self.scim.enable {
            return None;
        }
        Some(ScimConfig {
            token_file: self.scim.token_file.clone()?,
            hard_delete: self.scim.hard_delete,
        })
    }

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

    pub fn github_app_config(&self) -> Option<GitHubAppConfig> {
        Some(GitHubAppConfig {
            app_id: self.github_app.id?.get(),
            private_key_file: self.github_app.private_key_file.clone()?,
            webhook_secret_file: self.github_app.webhook_secret_file.clone()?,
        })
    }

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

    pub fn network_config(&self) -> Result<NetworkConfig, ConfigError> {
        Ok(NetworkConfig {
            trusted_proxies: super::cli::parse_cidr_list(&self.http.trusted_proxies)
                .map_err(ConfigError::TrustedProxies)?,
            local_ips: super::cli::parse_cidr_list(&self.http.local_ips)
                .map_err(ConfigError::LocalIps)?,
        })
    }

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

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub log: LogArgs,
    pub server: ServerArgs,
    pub secrets: SecretsArgs,
    pub state: StateArgs,
    pub permissions: PermissionsArgs,
    pub registration: RegistrationArgs,
    pub gradient_ci: GradientCiArgs,
    pub ssh: SshArgs,
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
    pub runner: RunnerArgs,
    pub oidc: Option<OidcConfig>,
    pub scim: Option<ScimConfig>,
    pub email: Option<EmailConfig>,
    pub s3: Option<S3Config>,
    pub github_app: Option<GitHubAppConfig>,
    pub metrics: Option<MetricsConfig>,
    /// These pipeline settings are always present. The `metrics` field is gating only the
    /// scrape token.
    pub metrics_args: MetricsArgs,
    pub network: NetworkConfig,
}

impl RuntimeConfig {
    pub fn from_cli(cli: &Cli) -> Result<Self, ConfigError> {
        Ok(Self {
            log: cli.log.clone(),
            server: cli.server.clone(),
            secrets: cli.secrets.clone(),
            state: cli.state.clone(),
            permissions: cli.permissions.clone(),
            registration: cli.registration.clone(),
            gradient_ci: cli.gradient_ci.clone(),
            ssh: cli.ssh.clone(),
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
            runner: cli.runner.clone(),
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
    use std::io::Write;

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
                max_connections: std::num::NonZeroUsize::new(16).expect("non-zero"),
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
    fn oidc_config_requires_enable_and_all_fields() {
        assert!(base_cli().oidc_config().is_none());

        let mut cli = base_cli();
        cli.oidc.enable = true;
        assert!(cli.oidc_config().is_none(), "enabled without fields");

        cli.oidc.client_id = Some("client-id".into());
        cli.oidc.client_secret_file = Some("/run/secrets/oidc".into());
        cli.oidc.discovery_url = Some("https://idp.example.com".into());
        assert_eq!(
            cli.oidc_config().expect("configured").client_id,
            "client-id"
        );
    }

    #[test]
    fn email_config_requires_enable_and_smtp_details() {
        assert!(base_cli().email_config().is_none());

        let mut cli = base_cli();
        cli.email.enable = true;
        assert!(cli.email_config().is_none(), "enabled without an smtp host");

        cli.email.smtp_host = Some("smtp.example.com".into());
        cli.email.smtp_username = Some("user".into());
        cli.email.smtp_password_file = Some("/run/secrets/smtp".into());
        cli.email.from_address = Some("gradient@example.com".into());
        let config = cli.email_config().expect("configured");
        assert_eq!(config.smtp_host, "smtp.example.com");
        assert_eq!(config.smtp_port, 587);
    }

    #[test]
    fn github_app_config_requires_all_three_fields() {
        assert!(base_cli().github_app_config().is_none());

        let mut cli = base_cli();
        cli.github_app.id = std::num::NonZeroU64::new(42);
        assert!(cli.github_app_config().is_none(), "id alone is not enough");

        cli.github_app.id = std::num::NonZeroU64::new(12345);
        cli.github_app.private_key_file = Some("/run/secrets/github-app.pem".into());
        cli.github_app.webhook_secret_file = Some("/run/secrets/github-webhook".into());
        let config = cli.github_app_config().expect("configured");
        assert_eq!(config.app_id, 12345);
        assert_eq!(config.private_key_file, "/run/secrets/github-app.pem");
        assert_eq!(config.webhook_secret_file, "/run/secrets/github-webhook");
    }

    #[test]
    fn s3_config_needs_a_bucket_and_fills_defaults() {
        assert!(base_cli().s3_config().is_none());

        let mut cli = base_cli();
        cli.s3.bucket = Some("my-bucket".into());
        let config = cli.s3_config().expect("configured");
        assert_eq!(config.bucket, "my-bucket");
        assert_eq!(config.region, "us-east-1");
        assert!(config.endpoint.is_none());
    }

    #[test]
    fn network_config_parses_cidrs_and_names_the_failing_list() {
        let cfg = base_cli()
            .network_config()
            .expect("default CIDR lists parse");
        assert!(!cfg.trusted_proxies.is_empty());
        assert!(!cfg.local_ips.is_empty());

        let mut cli = base_cli();
        cli.http.trusted_proxies = vec!["not-a-cidr".into()];
        assert!(
            cli.network_config()
                .unwrap_err()
                .to_string()
                .contains("GRADIENT_HTTP_TRUSTED_PROXIES")
        );

        let mut cli = base_cli();
        cli.http.local_ips = vec!["10.0.0.0/8".into(), "banana".into()];
        let err = cli.network_config().unwrap_err().to_string();
        assert!(err.contains("GRADIENT_HTTP_LOCAL_IPS"), "{err}");
        assert!(err.contains("banana"), "{err}");
    }

    #[test]
    fn metrics_config_reads_only_a_non_blank_token_file() {
        assert!(base_cli().metrics_config().is_none());

        let mut blank = tempfile::NamedTempFile::new().expect("tempfile");
        write!(blank, "   \n  ").expect("write");
        let mut cli = base_cli();
        cli.metrics.token_file = Some(blank.path().to_string_lossy().into_owned());
        assert!(cli.metrics_config().is_none(), "blank file is no config");

        let mut filled = tempfile::NamedTempFile::new().expect("tempfile");
        writeln!(filled, "  s3cret-token").expect("write");
        cli.metrics.token_file = Some(filled.path().to_string_lossy().into_owned());
        assert_eq!(cli.metrics_config().expect("Some").token, "s3cret-token");
    }

    #[test]
    fn scim_config_requires_enable_and_a_token_file() {
        assert!(base_cli().scim_config().is_none());

        let mut cli = base_cli();
        cli.scim.enable = true;
        assert!(cli.scim_config().is_none(), "enabled without a token file");

        cli.scim.token_file = Some("/run/secrets/scim".into());
        cli.scim.hard_delete = true;
        let cfg = cli.scim_config().expect("configured");
        assert_eq!(cfg.token_file, "/run/secrets/scim");
        assert!(cfg.hard_delete);
    }
}
