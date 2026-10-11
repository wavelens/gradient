/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[expect(dead_code, reason = "this binary only lists the options")]
#[path = "../src/config/options.rs"]
mod options;

use clap::CommandFactory;
use gradient_types::cli::options_json;
use options::WorkerConfig;

fn main() {
    print!(
        "{}",
        options_json(&WorkerConfig::command(), &WorkerConfig::options())
    );
}
