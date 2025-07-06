/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::config::*;
use connector::RequestConfig;
use rpassword::read_password;
use std::collections::HashMap;
use std::io::Write;
use std::process::Command;
use std::process::exit;
use std::{fs, io};

pub fn handle_input(values: Vec<(String, Option<String>)>, skip: bool) -> HashMap<String, String> {
    if values.is_empty() {
        println!("No input fields");
        exit(1);
    }

    if skip && !values.iter().any(|(_, v)| v.is_none()) {
        return values
            .iter()
            .map(|(k, v)| (k.clone(), v.clone().unwrap()))
            .collect();
    }

    let input_fields: String = values
        .iter()
        .map(|(k, v)| {
            format!(
                "{}: {}\n",
                k,
                if let Some(val) = v {
                    val.clone()
                } else {
                    "".to_string()
                }
            )
        })
        .collect();

    let name = format!("/tmp/GRADIENT-CONFIGURATOR-{}", std::process::id());

    let mut file = fs::File::create(name.clone()).unwrap();
    file.write_all(input_fields.as_bytes()).unwrap();

    let editor = std::env::var("EDITOR").unwrap();
    let output = Command::new(editor.clone())
        .arg(name.clone())
        .status()
        .unwrap();

    if !output.success() {
        println!("Failed to open editor {}", editor);
        exit(1);
    }

    let contents = fs::read_to_string(name.clone()).unwrap();
    fs::remove_file(name).unwrap();

    let mut result: HashMap<String, String> = HashMap::new();
    for line in contents.lines() {
        let parts: Vec<&str> = line.split(":").map(|v| v.trim()).collect();

        if !values.iter().any(|(k, _)| k == parts[0]) {
            eprintln!("Invalid input field: {}", parts[0]);
            exit(1);
        }

        if parts[1].is_empty() {
            eprintln!("{} cannot be empty.", parts[0]);
            exit(1);
        }

        result.insert(parts[0].to_string(), parts[1..].join(":").to_string());
    }

    result
}

pub fn ask_for_password() -> String {
    print!("Password: ");
    std::io::stdout().flush().unwrap();
    let inp = read_password().unwrap();

    if inp.is_empty() {
        eprintln!("Password cannot be empty.");
        exit(1);
    }

    inp
}

pub fn ask_for_input(prompt: &str) -> String {
    print!("{}: ", prompt);
    std::io::stdout().flush().unwrap();
    let mut inp = String::new();
    io::stdin()
        .read_line(&mut inp)
        .expect(format!("Failed to read {}.", prompt).as_str());
    let inp = inp.trim().to_string();

    if inp.is_empty() {
        eprintln!("{} cannot be empty.", prompt);
        exit(1);
    }

    inp
}

pub fn get_request_config(
    config: HashMap<ConfigKey, Option<String>>,
) -> Result<RequestConfig, String> {
    let server_url: String =
        if let Some(server_url) = config.get(&ConfigKey::Server).unwrap().clone() {
            server_url
        } else {
            return Err(
                "Server URL not set. Use `gradient config server <url>` to set it.".to_string(),
            );
        };

    let token = set_get_value(ConfigKey::AuthToken, None, true);

    Ok(RequestConfig { server_url, token })
}
