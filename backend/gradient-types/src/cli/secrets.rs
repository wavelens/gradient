/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

/// Both files are required to run the server but deliberately default to empty
/// so `--state-validate` (a DB-free, secret-free build/CI check) can parse
/// without them; `init_state` rejects an empty value on the live server path.
#[derive(Args, Debug, Clone, Default)]
pub struct SecretsArgs {
    #[arg(
        long = "secrets-crypt-file",
        env = "GRADIENT_SECRETS_CRYPT_FILE",
        default_value = ""
    )]
    pub crypt_file: String,
    #[arg(
        long = "secrets-jwt-file",
        env = "GRADIENT_SECRETS_JWT_FILE",
        default_value = ""
    )]
    pub jwt_file: String,
}

#[cfg(test)]
mod tests {
    use crate::Cli;
    use clap::Parser;

    #[test]
    fn validate_state_parses_without_secret_files() {
        let cli = Cli::try_parse_from([
            "gradient-server",
            "--state-file",
            "s.json",
            "--state-validate",
        ])
        .expect("--state-validate must parse without secret files");
        assert!(cli.state.validate);
    }

    #[test]
    fn secret_files_parse_from_flags() {
        let cli = Cli::try_parse_from([
            "gradient-server",
            "--secrets-crypt-file",
            "/c",
            "--secrets-jwt-file",
            "/j",
        ])
        .expect("explicit secret files must parse");
        assert_eq!(cli.secrets.crypt_file, "/c");
        assert_eq!(cli.secrets.jwt_file, "/j");
    }
}
