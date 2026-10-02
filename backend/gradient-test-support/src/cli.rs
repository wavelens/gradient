/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::Cli;
use gradient_types::cli::*;

pub fn test_cli() -> Cli {
    test_cli_with_crypt("test-secret".into())
}

pub fn test_cli_with_crypt(crypt_file: String) -> Cli {
    Cli {
        log: LogArgs {
            level_default: "error".into(),
            ..Default::default()
        },
        server: ServerArgs {
            serve_url: "http://127.0.0.1:3000".into(),
            use_tls: false,
            base_dir: tempfile::Builder::new()
                .prefix("gradient-test-")
                .tempdir()
                .expect("create test base_dir tempdir")
                .keep()
                .to_string_lossy()
                .into_owned(),
            ..Default::default()
        },
        build: BuildArgs {
            default_timeout_secs: 3600,
            default_max_silent_secs: 1800,
            ..Default::default()
        },
        secrets: SecretsArgs {
            crypt_file,
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
