/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod commands;
mod config;
mod input;
mod narinfo;
pub mod output;
mod tui;

use commands::base;

fn main() -> std::io::Result<()> {
    base::complete_env();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(base::run_cli())
}
