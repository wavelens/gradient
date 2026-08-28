/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use etcetera::base_strategy::{BaseStrategy, choose_base_strategy, choose_native_strategy};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::{fmt, fs};
use strum::IntoEnumIterator;
use strum_macros::EnumIter;

#[derive(Clone, Debug, EnumIter, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum ConfigKey {
    AuthToken,
    Server,
    SelectedProject,
    SelectedTask,
    SelectedBuild,
}

impl fmt::Display for ConfigKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", format!("{:?}", self).to_lowercase())
    }
}

impl std::str::FromStr for ConfigKey {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ConfigKey::iter()
            .find(|key| format!("{}", key).to_lowercase() == s.to_lowercase())
            .ok_or(())
    }
}

fn config_file_in(base: &Path) -> PathBuf {
    base.join("gradient").join("config.toml")
}

/// The XDG path wins, except when only a pre-XDG native config exists: macOS
/// puts the native config under `~/Library/Application Support`, which ignores
/// `XDG_CONFIG_HOME`, so installs made before #536 stay logged in.
fn resolve_config_file(xdg: PathBuf, native: PathBuf) -> PathBuf {
    if !xdg.exists() && native.exists() {
        native
    } else {
        xdg
    }
}

fn get_config_file() -> PathBuf {
    let xdg = config_file_in(
        &choose_base_strategy()
            .expect("Could not find configuration directory")
            .config_dir(),
    );

    match choose_native_strategy() {
        Ok(native) => resolve_config_file(xdg, config_file_in(&native.config_dir())),
        Err(_) => xdg,
    }
}

pub fn load_config() -> HashMap<ConfigKey, Option<String>> {
    let config_file = get_config_file();
    if config_file.exists() {
        let contents = fs::read_to_string(&config_file).expect("Failed to read configuration file");
        toml::from_str(&contents).expect("Failed to parse configuration file")
    } else {
        let mut config = HashMap::new();

        for config_key in ConfigKey::iter() {
            config.insert(config_key, None);
        }

        config
    }
}

pub fn load_config_quiet() -> HashMap<ConfigKey, Option<String>> {
    let config_file = get_config_file();
    fs::read_to_string(&config_file)
        .ok()
        .and_then(|contents| toml::from_str(&contents).ok())
        .unwrap_or_else(|| ConfigKey::iter().map(|key| (key, None)).collect())
}

pub fn save_config(config: &HashMap<ConfigKey, Option<String>>) {
    let config_file = get_config_file();
    let config_dir = config_file
        .parent()
        .expect("Failed to get configuration directory");

    fs::create_dir_all(config_dir).expect("Failed to create configuration directory");

    let contents = toml::to_string_pretty(config).expect("Failed to serialize configuration");
    let mut file = fs::File::create(config_file).expect("Failed to create configuration file");
    file.write_all(contents.as_bytes())
        .expect("Failed to write configuration file");
}

pub fn set_get_value_from_string(
    key: String,
    value: Option<String>,
    quiet: bool,
) -> Result<Option<String>, String> {
    let config_keys = ConfigKey::iter().collect::<Vec<_>>();

    for config_key in config_keys.clone() {
        if key.to_lowercase() == format!("{}", config_key).to_lowercase() {
            return Ok(set_get_value(config_key, value.clone(), quiet));
        }
    }

    if !quiet {
        println!("Invalid key: {}", key);
        println!("Valid keys are:");
        for config_key in config_keys {
            println!("{}", config_key);
        }
    }

    Err("Invalid key".to_string())
}

pub fn set_get_value(key: ConfigKey, value: Option<String>, quiet: bool) -> Option<String> {
    if let Some(value) = value.clone() {
        let mut config = load_config();
        config.remove(&key);
        config.insert(key.clone(), Some(value.clone()));
        save_config(&config);

        if !quiet {
            println!("{} set to \"{}\"", key, value);
        }

        Some(value)
    } else {
        let config = load_config();
        let found_values = config
            .iter()
            .map(
                |(config_key, value): (&ConfigKey, &Option<String>)| -> Option<String> {
                    if &key == config_key {
                        if value.is_some() && !value.clone().unwrap().is_empty() {
                            let value = value.clone().unwrap();
                            if !quiet {
                                println!("{}", value);
                            };

                            return Some(value.clone());
                        } else {
                            if !quiet {
                                println!("[unset]");
                            };

                            return None;
                        }
                    }

                    None
                },
            )
            .filter(|value| value.is_some())
            .collect::<Vec<_>>();

        if let Some(value) = found_values.first() {
            value.clone()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn touch(base: &Path) -> PathBuf {
        let file = config_file_in(base);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "Server = 'http://localhost'\n").unwrap();
        file
    }

    #[test]
    fn fresh_install_writes_the_xdg_path() {
        let xdg = TempDir::new().unwrap();
        let native = TempDir::new().unwrap();
        assert_eq!(
            resolve_config_file(config_file_in(xdg.path()), config_file_in(native.path())),
            config_file_in(xdg.path())
        );
    }

    #[test]
    fn xdg_config_wins_over_a_native_one() {
        let xdg = TempDir::new().unwrap();
        let native = TempDir::new().unwrap();
        touch(xdg.path());
        touch(native.path());
        assert_eq!(
            resolve_config_file(config_file_in(xdg.path()), config_file_in(native.path())),
            config_file_in(xdg.path())
        );
    }

    #[test]
    fn a_pre_xdg_native_config_keeps_being_used() {
        let xdg = TempDir::new().unwrap();
        let native = TempDir::new().unwrap();
        touch(native.path());
        assert_eq!(
            resolve_config_file(config_file_in(xdg.path()), config_file_in(native.path())),
            config_file_in(native.path())
        );
    }
}
