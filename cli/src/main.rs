/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod commands;
mod config;
mod input;

use commands::base;

#[tokio::main]
pub async fn main() -> std::io::Result<()> {
    base::run_cli().await
}
