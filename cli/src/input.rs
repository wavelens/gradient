/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::config::*;
use crate::error::{CliError, FileAction};
use crate::output::{ExitKind, Output};
use connector::Client;
use rpassword::read_password;
use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::process::Command;
use std::{fs, io};

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnOffSetting {
    On,
    Off,
}

pub fn selected_project(out: Output) -> String {
    get_value(ConfigKey::SelectedProject, true)
        .unwrap_or_else(|| out.fail(CliError::NoProjectSelected))
}

pub fn selected_task(out: Output) -> (String, String) {
    selected_task_ids().unwrap_or_else(|| out.fail(CliError::NoTaskSelected))
}

pub fn server_base(out: Output) -> String {
    saved_login_for_invocation()
        .server()
        .unwrap_or_else(|| match account_name_for_command() {
            Err(e) => out.fail(e),
            Ok(_) => out.fail(CliError::ServerNotSet),
        })
}

pub fn client_from_config(out: Output) -> Client {
    let server = server_base(out);
    let token = saved_login_for_invocation().token();
    if token.is_some() && is_unencrypted_http_url_to_non_loopback_host(&server) {
        out.progress(format!(
            "Warning: {server} is plain http. Requests carry the API token unencrypted."
        ));
    }
    let mut b = Client::builder().base_url(server);
    if let Some(t) = token {
        b = b.token(t);
    }
    b.build()
        .unwrap_or_else(|e| out.err(ExitKind::Api, format!("client init failed: {}", e)))
}

fn is_unencrypted_http_url_to_non_loopback_host(server: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(server) else {
        return false;
    };
    let loopback = match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback()),
        None => false,
    };
    url.scheme() == "http" && !loopback
}

pub struct Inputs {
    values: HashMap<String, String>,
    out: Output,
}

impl Inputs {
    pub fn get(&self, field: &str) -> String {
        self.values
            .get(field)
            .cloned()
            .unwrap_or_else(|| self.out.fail(CliError::EmptyField(field.to_owned())))
    }
}

pub struct CommandLineFormField {
    label: &'static str,
    command_line_flag_value: Option<String>,
    current_value: Option<String>,
    can_be_empty: bool,
}

impl CommandLineFormField {
    pub fn new_non_empty(label: &'static str, command_line_flag_value: Option<String>) -> Self {
        Self {
            label,
            command_line_flag_value,
            current_value: None,
            can_be_empty: false,
        }
    }

    pub fn new_optional_form_field(
        label: &'static str,
        command_line_flag_value: Option<String>,
    ) -> Self {
        Self {
            can_be_empty: true,
            ..Self::new_non_empty(label, command_line_flag_value)
        }
    }

    pub fn default_value(self, current_value: impl Into<String>) -> Self {
        Self {
            current_value: Some(current_value.into()),
            ..self
        }
    }

    fn value(&self) -> Option<&String> {
        self.command_line_flag_value
            .as_ref()
            .or(self.current_value.as_ref())
    }

    fn is_missing(&self) -> bool {
        !self.can_be_empty && self.value().is_none_or(|value| value.is_empty())
    }
}

/// Open `$EDITOR` only in a terminal. Do so when no flags are present, or required fields are still missing.
pub fn handle_input(
    fields: Vec<CommandLineFormField>,
    has_non_form_field_command_line_flag: bool,
    out: Output,
) -> Inputs {
    if fields.is_empty() {
        out.err(ExitKind::Usage, "No input fields.");
    }

    let any_flag_given = has_non_form_field_command_line_flag
        || fields
            .iter()
            .any(|field| field.command_line_flag_value.is_some());
    let any_field_missing = fields.iter().any(CommandLineFormField::is_missing);
    let interactive = !out.is_json() && io::stdin().is_terminal();

    if !interactive || (any_flag_given && !any_field_missing) {
        if let Some(field) = fields.iter().find(|field| field.is_missing()) {
            let flag = field.label.to_lowercase().replace(' ', "-");
            out.err(ExitKind::Usage, format!("No argument: --{flag}."));
        }
        let values = fields
            .iter()
            .map(|field| {
                (
                    field.label.to_owned(),
                    field.value().cloned().unwrap_or_default(),
                )
            })
            .collect();
        return Inputs { values, out };
    }

    let draft: String = fields
        .iter()
        .map(|field| {
            format!(
                "{}: {}\n",
                field.label,
                field.value().map(String::as_str).unwrap_or_default()
            )
        })
        .collect();

    let editor =
        std::env::var("EDITOR").unwrap_or_else(|_| out.err(ExitKind::Usage, "EDITOR is not set."));

    let mut draft_file = tempfile::Builder::new()
        .prefix("gradient-form-")
        .tempfile()
        .unwrap_or_else(|e| out.fail(CliError::file(FileAction::Write, std::env::temp_dir(), e)));
    let path = draft_file.path().to_path_buf();
    draft_file
        .write_all(draft.as_bytes())
        .and_then(|()| draft_file.flush())
        .unwrap_or_else(|e| out.fail(CliError::file(FileAction::Write, &path, e)));

    let status = Command::new(&editor)
        .arg(&path)
        .status()
        .unwrap_or_else(|e| {
            out.err(
                ExitKind::Usage,
                format!("Failed to open editor {}: {}", editor, e),
            )
        });

    if !status.success() {
        out.err(ExitKind::Usage, format!("Failed to open editor {}", editor));
    }

    let edited = fs::read_to_string(&path)
        .unwrap_or_else(|e| out.fail(CliError::file(FileAction::Read, &path, e)));

    let values = parse_edited(&fields, &edited).unwrap_or_else(|e| out.err(ExitKind::Usage, e));

    Inputs { values, out }
}

fn parse_edited(
    fields: &[CommandLineFormField],
    edited: &str,
) -> Result<HashMap<String, String>, String> {
    let mut values: HashMap<String, String> = HashMap::new();

    for line in edited.lines().filter(|line| !line.trim().is_empty()) {
        let (label, value) = line
            .split_once(':')
            .ok_or_else(|| format!("Invalid input line: {}", line))?;
        let (label, value) = (label.trim(), value.trim());

        let field = fields
            .iter()
            .find(|field| field.label == label)
            .ok_or_else(|| format!("Invalid input field: {}", label))?;

        if value.is_empty() && !field.can_be_empty {
            return Err(CliError::EmptyField(label.to_owned()).to_string());
        }

        values.insert(label.to_string(), value.to_string());
    }

    for field in fields {
        if !values.contains_key(field.label) {
            if !field.can_be_empty {
                return Err(CliError::EmptyField(field.label.to_owned()).to_string());
            }
            values.insert(field.label.to_owned(), String::new());
        }
    }
    Ok(values)
}

pub fn confirm_delete(question: &str, yes: bool, out: Output) -> bool {
    if yes {
        return true;
    }
    if out.is_json() || !io::stdin().is_terminal() {
        out.err(
            ExitKind::Usage,
            "Specify --yes to delete without a confirmation prompt.",
        );
    }
    eprint!("{question} [y/N]: ");
    io::stderr().flush().ok();
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .unwrap_or_else(|e| out.fail(CliError::ReadInput(question.to_owned(), e)));
    let confirmed = matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes");
    if !confirmed {
        out.human("Cancelled.");
    }
    confirmed
}

pub fn ask_for_secret(prompt: &str, out: Output) -> String {
    if out.is_json() {
        out.fail(CliError::EmptyField(prompt.to_owned()));
    }
    print!("{}: ", prompt);
    flush_stdout(out);

    let input =
        read_password().unwrap_or_else(|e| out.fail(CliError::ReadInput(prompt.to_owned(), e)));

    if input.is_empty() {
        out.fail(CliError::EmptyField(prompt.to_owned()));
    }

    input
}

pub fn ask_for_password(out: Output) -> String {
    ask_for_secret("Password", out)
}

pub fn ask_for_input(prompt: &str, out: Output) -> String {
    if out.is_json() {
        out.fail(CliError::EmptyField(prompt.to_owned()));
    }
    print!("{}: ", prompt);
    flush_stdout(out);

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .unwrap_or_else(|e| out.fail(CliError::ReadInput(prompt.to_owned(), e)));

    let input = input.trim().to_string();
    if input.is_empty() {
        out.fail(CliError::EmptyField(prompt.to_owned()));
    }

    input
}

pub fn is_interactive(out: Output) -> bool {
    !out.is_json() && io::stdin().is_terminal() && io::stdout().is_terminal()
}

pub fn ask_for_choice(prompt: &str, options: &[String], out: Output) -> Option<String> {
    for (i, option) in options.iter().enumerate() {
        out.human(format!("  {}) {}", i + 1, option));
    }
    loop {
        print!("{} [1-{}, empty to skip]: ", prompt, options.len());
        flush_stdout(out);

        let mut input = String::new();
        let read = io::stdin()
            .read_line(&mut input)
            .unwrap_or_else(|e| out.fail(CliError::ReadInput(prompt.to_owned(), e)));
        if read == 0 {
            return None;
        }

        match parse_choice(&input, options) {
            Ok(choice) => return choice,
            Err(e) => out.human(e),
        }
    }
}

fn parse_choice(input: &str, options: &[String]) -> Result<Option<String>, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    let by_index = input
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .and_then(|i| options.get(i));
    by_index
        .or_else(|| options.iter().find(|o| *o == input))
        .map(|o| Some(o.clone()))
        .ok_or_else(|| format!("Invalid choice: {}", input))
}

pub fn flush_stdout(out: Output) {
    io::stdout()
        .flush()
        .unwrap_or_else(|e| out.err(ExitKind::Api, format!("Failed to write to stdout: {}", e)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(names: &[&'static str]) -> Vec<CommandLineFormField> {
        names
            .iter()
            .map(|n| CommandLineFormField::new_non_empty(n, None))
            .collect()
    }

    #[test]
    fn only_plain_http_to_a_remote_host_is_unencrypted() {
        assert!(is_unencrypted_http_url_to_non_loopback_host(
            "http://ci.example"
        ));
        assert!(!is_unencrypted_http_url_to_non_loopback_host(
            "https://ci.example"
        ));
        assert!(!is_unencrypted_http_url_to_non_loopback_host(
            "http://localhost:3000"
        ));
        assert!(!is_unencrypted_http_url_to_non_loopback_host(
            "http://127.0.0.1"
        ));
        assert!(!is_unencrypted_http_url_to_non_loopback_host(
            "http://[::1]:8080"
        ));
    }

    #[test]
    fn a_choice_by_number_picks_that_option() {
        let options = vec!["a".to_string(), "b".to_string()];
        assert_eq!(parse_choice(" 2\n", &options), Ok(Some("b".into())));
    }

    #[test]
    fn a_choice_by_name_picks_that_option() {
        let options = vec!["a".to_string(), "b".to_string()];
        assert_eq!(parse_choice("a\n", &options), Ok(Some("a".into())));
    }

    #[test]
    fn an_empty_choice_skips() {
        let options = vec!["a".to_string()];
        assert_eq!(parse_choice("\n", &options), Ok(None));
    }

    #[test]
    fn an_out_of_range_choice_is_rejected() {
        let options = vec!["a".to_string(), "b".to_string()];
        assert_eq!(parse_choice("0", &options), Err("Invalid choice: 0".into()));
        assert_eq!(parse_choice("3", &options), Err("Invalid choice: 3".into()));
        assert_eq!(parse_choice("c", &options), Err("Invalid choice: c".into()));
    }

    #[test]
    fn a_value_keeps_the_colons_inside_it() {
        let parsed =
            parse_edited(&fields(&["Repository"]), "Repository: https://x/y.git\n").unwrap();
        assert_eq!(parsed["Repository"], "https://x/y.git");
    }

    #[test]
    fn a_deleted_line_is_rejected_not_silently_missing() {
        let err = parse_edited(&fields(&["Name", "Description"]), "Name: gradient\n").unwrap_err();
        assert_eq!(err, "Description cannot be empty.");
    }

    #[test]
    fn an_emptied_value_is_rejected() {
        let err = parse_edited(&fields(&["Name"]), "Name:   \n").unwrap_err();
        assert_eq!(err, "Name cannot be empty.");
    }

    #[test]
    fn an_optional_field_may_be_emptied_or_deleted() {
        let fields = vec![
            CommandLineFormField::new_non_empty("Name", None),
            CommandLineFormField::new_optional_form_field("Description", None),
            CommandLineFormField::new_optional_form_field("Wildcard", None),
        ];
        let parsed = parse_edited(&fields, "Name: a\nDescription:\n").unwrap();
        assert_eq!(parsed["Description"], "");
        assert_eq!(parsed["Wildcard"], "");
    }

    #[test]
    fn a_required_field_is_missing_without_flag_or_current_value() {
        assert!(CommandLineFormField::new_non_empty("Name", None).is_missing());
        assert!(CommandLineFormField::new_non_empty("Name", Some(String::new())).is_missing());
        assert!(
            !CommandLineFormField::new_non_empty("Name", None)
                .default_value("a")
                .is_missing()
        );
        assert!(!CommandLineFormField::new_optional_form_field("Description", None).is_missing());
    }

    #[test]
    fn an_undeclared_field_is_rejected() {
        let err = parse_edited(&fields(&["Name"]), "Name: a\nNickname: b\n").unwrap_err();
        assert_eq!(err, "Invalid input field: Nickname");
    }

    #[test]
    fn a_line_without_a_separator_is_rejected() {
        let err = parse_edited(&fields(&["Name"]), "Name\n").unwrap_err();
        assert_eq!(err, "Invalid input line: Name");
    }
}
