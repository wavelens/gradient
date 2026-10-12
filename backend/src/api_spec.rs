/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_web::api_spec::{SpecError, api_spec, api_spec_yaml};
use gradient_web::api_typescript::typescript;
use std::process::ExitCode;

fn output(format: Option<&str>) -> Result<String, SpecError> {
    match format {
        Some("typescript") => Ok(typescript(&api_spec()?)),
        _ => api_spec_yaml(),
    }
}

fn main() -> ExitCode {
    match output(std::env::args().nth(1).as_deref()) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
