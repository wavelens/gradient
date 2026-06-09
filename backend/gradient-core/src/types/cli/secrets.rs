/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

/// Both files are required to run the server but deliberately default to empty
/// so `--validate-state` (a DB-free, secret-free build/CI check) can parse
/// without them; `init_state` rejects an empty value on the live server path.
#[derive(Args, Debug, Clone, Default)]
pub struct SecretsArgs {
    #[arg(long, env = "GRADIENT_CRYPT_SECRET_FILE", default_value = "")]
    pub crypt_secret_file: String,
    #[arg(long, env = "GRADIENT_JWT_SECRET_FILE", default_value = "")]
    pub jwt_secret_file: String,
}

#[cfg(test)]
mod tests {
    use crate::types::Cli;
    use clap::Parser;

    #[test]
    fn validate_state_parses_without_secret_files() {
        let cli = Cli::try_parse_from(["gradient-server", "--state-file", "s.json", "--validate-state"])
            .expect("--validate-state must parse without secret files");
        assert!(cli.storage.validate_state);
    }

    #[test]
    fn secret_files_parse_from_flags() {
        let cli =
            Cli::try_parse_from(["gradient-server", "--crypt-secret-file", "/c", "--jwt-secret-file", "/j"])
                .expect("explicit secret files must parse");
        assert_eq!(cli.secrets.crypt_secret_file, "/c");
        assert_eq!(cli.secrets.jwt_secret_file, "/j");
    }
}
