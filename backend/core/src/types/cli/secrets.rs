/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct SecretsArgs {
    #[arg(long, env = "GRADIENT_CRYPT_SECRET_FILE")]
    pub crypt_secret_file: String,
    #[arg(long, env = "GRADIENT_JWT_SECRET_FILE")]
    pub jwt_secret_file: String,
}
