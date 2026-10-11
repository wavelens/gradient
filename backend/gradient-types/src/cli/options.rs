/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct OptionSpec {
    pub group: &'static str,
    pub field: &'static str,
    pub flag: &'static str,
    pub env: Option<&'static str>,
    #[serde(rename = "type")]
    pub rust_type: &'static str,
    pub description: &'static str,
}

#[derive(Serialize)]
struct OptionEntry<'a> {
    #[serde(flatten)]
    spec: &'a OptionSpec,
    default: Option<String>,
    values: Vec<String>,
}

pub fn options_json(command: &clap::Command, specs: &[OptionSpec]) -> String {
    let entries: Vec<OptionEntry<'_>> = specs
        .iter()
        .map(|spec| {
            let arg = command
                .get_arguments()
                .find(|arg| arg.get_long() == Some(spec.flag))
                .unwrap_or_else(|| panic!("--{} has no argument", spec.flag));

            OptionEntry {
                spec,
                default: arg
                    .get_default_values()
                    .first()
                    .map(|value| value.to_string_lossy().into_owned()),
                values: arg
                    .get_possible_values()
                    .iter()
                    .map(|value| value.get_name().to_owned())
                    .collect(),
            }
        })
        .collect();

    let mut json = serde_json::to_string_pretty(&entries).expect("option entries are plain data");
    json.push('\n');
    json
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cli;
    use clap::CommandFactory;

    #[test]
    fn checked_in_options_file_is_current() {
        let current = options_json(&Cli::command(), &Cli::options());

        assert_eq!(
            current,
            include_str!("../../options.json"),
            "run `cargo run -p gradient-types --example options > gradient-types/options.json`"
        );
    }
}
