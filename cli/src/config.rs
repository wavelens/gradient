/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */

use std::path::PathBuf;
use std::{fs, fmt};
use std::io::Write;
use std::collections::HashMap;
use strum::IntoEnumIterator;
use strum_macros::EnumIter;
use serde::{Serialize, Deserialize};

#[derive(Clone, Debug, EnumIter, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum ConfigKey {
    AuthToken,
    Server,
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

fn get_config_file() -> PathBuf {
    let mut config_dir = dirs::config_dir().expect("Could not find configuration directory");
    config_dir.push("gradient");
    config_dir.push("config.toml");
    config_dir
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

pub fn save_config(config: &HashMap<ConfigKey, Option<String>>) {
    let config_file = get_config_file();
    let config_dir = config_file.parent().expect("Failed to get configuration directory");

    // Create the directory if it doesn't exist
    fs::create_dir_all(config_dir).expect("Failed to create configuration directory");

    // Write the configuration to the file
    let contents = toml::to_string_pretty(config).expect("Failed to serialize configuration");
    let mut file = fs::File::create(config_file).expect("Failed to create configuration file");
    file.write_all(contents.as_bytes())
        .expect("Failed to write configuration file");
}

pub fn set_get_value(key: String, value: Option<String>, quiet: bool) -> Result<Option<String>, String> {
    let config_keys = ConfigKey::iter().collect::<Vec<_>>();

    for config_key in config_keys.clone() {
        if key.to_lowercase() == format!("{}", config_key).to_lowercase() {
            if let Some(value) = value.clone() {
                let mut config = load_config();
                config.remove(&config_key);
                config.insert(config_key.clone(), Some(value.clone()));
                save_config(&config);

                if !quiet {
                    println!("{} set to \"{}\"", config_key, value);
                }
            } else {
                let config = load_config();
                let found_values = config.iter().map(|(key, value): (&ConfigKey, &Option<String>)| -> Option<String> {
                    if key == &config_key {
                        if let Some(value) = value {
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
                })
                .filter(|value| value.is_some())
                .collect::<Vec<_>>();

                if let Some(value) = found_values.first() {
                    return Ok(value.clone());
                } else {
                    return Ok(None);
                }
            }

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
