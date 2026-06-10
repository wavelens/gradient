/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::Cli;
use gradient_types::cli::*;

/// Single source of truth for the `Cli` struct in tests.
/// Update only here when fields are added/removed from `Cli`.
pub fn test_cli() -> Cli {
    test_cli_with_crypt("test-secret".into())
}

/// Like `test_cli()` but with a custom `crypt_secret_file` path.
/// Use this in tests that need a real decryptable webhook secret.
pub fn test_cli_with_crypt(crypt_secret_file: String) -> Cli {
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
            build_max_attempts: 3,
            substitute_miss_escalation_threshold: 2,
            build_retry_backoff_secs: 30,
            build_default_timeout_secs: 3600,
            build_default_max_silent_secs: 1800,
            scheduler_scoring_policy: "resource-aware".into(),
        },
        storage: StorageArgs {
            base_path: tempfile::Builder::new()
                .prefix("gradient-test-")
                .tempdir()
                .expect("create test base_path tempdir")
                .keep()
                .to_string_lossy()
                .into_owned(),
            keep_evaluations: 30,
            ..Default::default()
        },
        secrets: SecretsArgs {
            crypt_secret_file,
            jwt_secret_file: "test-jwt".into(),
        },
        limits: LimitsArgs::default(),
        registration: RegistrationArgs {
            enable_registration: false,
            report_errors: false,
            sentry_dsn: None,
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
        metrics: MetricsArgs::default(),
        network: NetworkArgs::default(),
    }
}
