/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::Path;

use anyhow::Result;
use gradient_wire::guest::ContextLayers;

use crate::setup::{give_to_program_user, write_file};

const CLI_CONFIG_FILE: &str = "/home/gradient/.config/gradient/config.toml";

pub fn cli_config_toml(api: &str, token: &str, project: &str, task: &str) -> String {
    let entries = [
        ("Server", api.to_owned()),
        ("AuthToken", token.to_owned()),
        ("SelectedProject", project.to_owned()),
        ("SelectedTask", format!("{project}/{task}")),
    ];
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), toml::Value::String(value)))
        .collect::<toml::Table>()
        .to_string()
}

pub async fn write_cli_config(layers: &ContextLayers) -> Result<()> {
    let value = |key: &str| layers.job.get(key).map_or("", String::as_str);
    let config = cli_config_toml(
        value("api"),
        value("token"),
        value("project_name"),
        value("task_name"),
    );
    let file = Path::new(CLI_CONFIG_FILE);
    write_file(file, config.as_bytes(), 0o600).await?;
    if let Some(gradient_dir) = file.parent() {
        give_to_program_user(gradient_dir, 0o700).await?;
        if let Some(config_dir) = gradient_dir.parent() {
            give_to_program_user(config_dir, 0o755).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_config_points_the_cli_at_the_job() {
        let config: toml::Table = toml::from_str(&cli_config_toml(
            "https://ci.example.com",
            "GRJ_x",
            "infra",
            "master",
        ))
        .unwrap();
        assert_eq!(config["Server"].as_str(), Some("https://ci.example.com"));
        assert_eq!(config["AuthToken"].as_str(), Some("GRJ_x"));
        assert_eq!(config["SelectedProject"].as_str(), Some("infra"));
        assert_eq!(config["SelectedTask"].as_str(), Some("infra/master"));
    }
}
