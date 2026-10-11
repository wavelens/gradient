/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::CommandFactory;
use gradient_types::Cli;
use gradient_types::cli::options_json;

fn main() {
    print!("{}", options_json(&Cli::command(), &Cli::options()));
}
