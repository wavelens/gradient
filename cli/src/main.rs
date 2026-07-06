/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod commands;
mod config;
mod input;
mod narinfo;
mod netrc;
pub mod output;
mod tui;

use commands::base;

fn main() -> std::io::Result<()> {
    base::run()
}
