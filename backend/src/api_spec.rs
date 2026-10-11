/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::process::ExitCode;

fn main() -> ExitCode {
    match gradient_web::api_spec::api_spec_yaml() {
        Ok(spec) => {
            print!("{spec}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
