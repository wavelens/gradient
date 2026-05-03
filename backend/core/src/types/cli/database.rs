/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone, Default)]
pub struct DatabaseArgs {
    #[arg(long, env = "GRADIENT_DATABASE_URL")]
    pub database_url: Option<String>,
    #[arg(long, env = "GRADIENT_DATABASE_URL_FILE")]
    pub database_url_file: Option<String>,
}
