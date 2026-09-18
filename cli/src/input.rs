/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::config::*;
use crate::output::{ExitKind, Output};
use connector::Client;
use rpassword::read_password;
use std::collections::HashMap;
use std::io::Write;
use std::process::Command;
use std::{fs, io};

pub fn client_from_config(out: Output) -> Client {
    let cfg = load_config();
    let server = cfg
        .get(&ConfigKey::Server)
        .and_then(|v| v.clone())
        .unwrap_or_else(|| {
            out.err(
                ExitKind::Usage,
                "Server URL not set. Run `gradient login <url>` to set the server and authenticate.",
            );
        });
    let token = cfg
        .get(&ConfigKey::AuthToken)
        .and_then(|v| v.clone())
        .filter(|t| !t.is_empty());
    let mut b = Client::builder().base_url(server);
    if let Some(t) = token {
        b = b.token(t);
    }
    b.build()
        .unwrap_or_else(|e| out.err(ExitKind::Api, format!("client init failed: {}", e)))
}

#[cfg(feature = "nix")]
pub fn server_base(out: Output) -> String {
    load_config()
        .get(&ConfigKey::Server)
        .and_then(|v| v.clone())
        .unwrap_or_else(|| {
            out.err(
                ExitKind::Usage,
                "Server URL not set. Run `gradient login <url>` to set the server and authenticate.",
            );
        })
}

/// The fields a command asked for, keyed by prompt label. `handle_input` hands
/// one out only once every declared field carries a value, so lookups cannot miss.
pub struct Inputs {
    values: HashMap<String, String>,
    out: Output,
}

impl Inputs {
    pub fn get(&self, field: &str) -> String {
        self.values.get(field).cloned().unwrap_or_else(|| {
            self.out
                .err(ExitKind::Usage, format!("{} cannot be empty.", field))
        })
    }
}

pub fn handle_input(fields: Vec<(String, Option<String>)>, skip: bool, out: Output) -> Inputs {
    if fields.is_empty() {
        out.err(ExitKind::Usage, "No input fields.");
    }

    if skip {
        let prefilled: Option<HashMap<String, String>> = fields
            .iter()
            .map(|(field, value)| value.clone().map(|value| (field.clone(), value)))
            .collect();

        if let Some(values) = prefilled {
            return Inputs { values, out };
        }
    }

    let draft: String = fields
        .iter()
        .map(|(field, value)| format!("{}: {}\n", field, value.clone().unwrap_or_default()))
        .collect();

    let path = std::env::temp_dir().join(format!("gradient-configurator-{}", std::process::id()));
    let editor =
        std::env::var("EDITOR").unwrap_or_else(|_| out.err(ExitKind::Usage, "EDITOR is not set."));

    fs::write(&path, draft).unwrap_or_else(|e| {
        out.err(
            ExitKind::Api,
            format!("Failed to write {}: {}", path.display(), e),
        )
    });

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

    let edited = fs::read_to_string(&path).unwrap_or_else(|e| {
        out.err(
            ExitKind::Api,
            format!("Failed to read {}: {}", path.display(), e),
        )
    });
    let _ = fs::remove_file(&path);

    let values = parse_edited(&fields, &edited).unwrap_or_else(|e| out.err(ExitKind::Usage, e));

    Inputs { values, out }
}

/// Read the editor's buffer back. A field the user emptied or deleted is an
/// error here rather than a missing key the caller would have to handle.
fn parse_edited(
    fields: &[(String, Option<String>)],
    edited: &str,
) -> Result<HashMap<String, String>, String> {
    let mut values: HashMap<String, String> = HashMap::new();

    for line in edited.lines().filter(|line| !line.trim().is_empty()) {
        let (field, value) = line
            .split_once(':')
            .ok_or_else(|| format!("Invalid input line: {}", line))?;
        let (field, value) = (field.trim(), value.trim());

        if !fields.iter().any(|(declared, _)| declared == field) {
            return Err(format!("Invalid input field: {}", field));
        }

        if value.is_empty() {
            return Err(format!("{} cannot be empty.", field));
        }

        values.insert(field.to_string(), value.to_string());
    }

    match fields.iter().find(|(field, _)| !values.contains_key(field)) {
        Some((missing, _)) => Err(format!("{} cannot be empty.", missing)),
        None => Ok(values),
    }
}

pub fn ask_for_password(out: Output) -> String {
    print!("Password: ");
    flush_stdout(out);

    let input = read_password()
        .unwrap_or_else(|e| out.err(ExitKind::Usage, format!("Failed to read password: {}", e)));

    if input.is_empty() {
        out.err(ExitKind::Usage, "Password cannot be empty.");
    }

    input
}

pub fn ask_for_input(prompt: &str, out: Output) -> String {
    print!("{}: ", prompt);
    flush_stdout(out);

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .unwrap_or_else(|e| out.err(ExitKind::Usage, format!("Failed to read {}: {}", prompt, e)));

    let input = input.trim().to_string();
    if input.is_empty() {
        out.err(ExitKind::Usage, format!("{} cannot be empty.", prompt));
    }

    input
}

pub fn flush_stdout(out: Output) {
    io::stdout()
        .flush()
        .unwrap_or_else(|e| out.err(ExitKind::Api, format!("Failed to write to stdout: {}", e)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(names: &[&str]) -> Vec<(String, Option<String>)> {
        names.iter().map(|n| (n.to_string(), None)).collect()
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
