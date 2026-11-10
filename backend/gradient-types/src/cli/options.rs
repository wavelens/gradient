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
    #[serde(skip)]
    pub secret: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServerOptionValueSource {
    Flag,
    Env,
    Default,
    Unset,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct EffectiveConfigOption {
    pub group: String,
    pub field: String,
    pub flag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub secret: bool,
    pub source: ServerOptionValueSource,
}

pub fn collect_active_options_with_secret_values_hidden(
    command: &clap::Command,
    matches: &clap::ArgMatches,
    specs: &[OptionSpec],
) -> Vec<EffectiveConfigOption> {
    specs
        .iter()
        .map(|spec| {
            let id = command
                .get_arguments()
                .find(|arg| arg.get_long() == Some(spec.flag))
                .unwrap_or_else(|| panic!("--{} has no argument", spec.flag))
                .get_id();
            let source = match matches.value_source(id.as_str()) {
                Some(clap::parser::ValueSource::CommandLine) => ServerOptionValueSource::Flag,
                Some(clap::parser::ValueSource::EnvVariable) => ServerOptionValueSource::Env,
                Some(_) => ServerOptionValueSource::Default,
                None => ServerOptionValueSource::Unset,
            };
            let value = matches.get_raw(id.as_str()).map(|values| {
                values
                    .map(|value| value.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(",")
            });

            EffectiveConfigOption {
                group: spec.group.to_owned(),
                field: spec.field.to_owned(),
                flag: spec.flag.to_owned(),
                env: spec.env.map(str::to_owned),
                value: value.filter(|_| !spec.secret),
                secret: spec.secret,
                source,
            }
        })
        .collect()
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

    fn option<'a>(options: &'a [EffectiveConfigOption], flag: &str) -> &'a EffectiveConfigOption {
        options
            .iter()
            .find(|option| option.flag == flag)
            .unwrap_or_else(|| panic!("--{flag} is listed"))
    }

    #[test]
    fn each_option_names_its_source_and_secrets_carry_no_value() {
        let command = Cli::command().mut_args(|arg| arg.env(None));
        let matches = command.clone().get_matches_from([
            "gradient-server",
            "--database-url",
            "postgres://gradient:hunter2@db/gradient",
            "--eval-max-keep",
            "7",
        ]);

        let options =
            collect_active_options_with_secret_values_hidden(&command, &matches, &Cli::options());

        let database = option(&options, "database-url");
        assert_eq!(database.source, ServerOptionValueSource::Flag);
        assert!(database.secret);
        assert_eq!(database.value, None);
        let keep = option(&options, "eval-max-keep");
        assert_eq!(
            (keep.source, keep.value.as_deref()),
            (ServerOptionValueSource::Flag, Some("7"))
        );
        let sweep = option(&options, "cache-sign-sweep-interval-secs");
        assert_eq!(
            (sweep.source, sweep.value.as_deref()),
            (ServerOptionValueSource::Default, Some("3600"))
        );
        assert_eq!(
            option(&options, "sentry-dsn").source,
            ServerOptionValueSource::Unset
        );
        assert!(
            !serde_json::to_string(&options)
                .expect("options serialize")
                .contains("hunter2")
        );
    }
}
