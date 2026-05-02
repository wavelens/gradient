/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::*;
use crate::input::*;
use clap::Subcommand;
use connector::*;
use std::fs;
use std::process::exit;

#[derive(Subcommand, Debug)]
pub enum Commands {
    Create {
        #[arg(short, long)]
        name: Option<String>,
        #[arg(short, long)]
        display_name: Option<String>,
        #[arg(short = 'c', long)]
        description: Option<String>,
        #[arg(short, long)]
        priority: Option<i32>,
    },
    List,
    Edit {
        name: String,
        #[arg(short, long)]
        display_name: Option<String>,
        #[arg(short = 'c', long)]
        description: Option<String>,
        #[arg(short, long)]
        priority: Option<i32>,
    },
    Delete {
        name: String,
    },
    Show {
        name: String,
    },
    /// Build a netrc entry locally for a cache and install it into a netrc file.
    /// No request is sent to the server — bring your own API key (issued via the
    /// frontend or `gradient` UI). Intended to be run as root (e.g. sudo) to
    /// install into /etc/nix/netrc.
    InstallNetrc {
        /// Gradient server URL (e.g. https://gradient.example.com)
        #[arg(short, long)]
        server: String,
        /// Bearer token (`GRAD…` API key or JWT). Prompted on stdin if omitted.
        #[arg(short, long)]
        token: Option<String>,
        /// Name of the cache (used purely as a label in the netrc entry)
        #[arg(short, long)]
        cache: String,
        /// Path to the netrc file to update (default: /etc/nix/netrc)
        #[arg(short = 'f', long, default_value = "/etc/nix/netrc")]
        netrc_file: String,
    },
}

pub async fn handle(cmd: Commands) {
    match cmd {
        Commands::Create {
            name,
            display_name,
            description,
            priority,
        } => {
            let input_fields = [
                ("Name", name),
                ("Display Name", display_name),
                ("Description", description),
                ("Priority", priority.map(|p| p.to_string())),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

            let input = handle_input(input_fields, true);
            let name = input.get("Name").unwrap().clone();

            let priority = match input.get("Priority").unwrap().parse::<i32>() {
                Ok(p) => p,
                Err(_) => {
                    eprintln!("Priority must be an integer.");
                    exit(1);
                }
            };

            let res = caches::put(
                get_request_config(load_config()).unwrap(),
                name,
                input.get("Display Name").unwrap().clone(),
                input.get("Description").unwrap().clone(),
                priority,
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if res.error {
                eprintln!("Cache creation failed: {}", res.message);
                exit(1);
            }

            println!("Cache created.");
        }

        Commands::List => {
            let res = caches::get(get_request_config(load_config()).unwrap())
                .await
                .map_err(|e| {
                    eprintln!("{}", e);
                    exit(1);
                })
                .unwrap();

            if res.error {
                eprintln!("Failed to list caches");
                exit(1);
            }

            if res.message.is_empty() {
                println!("You have no caches.");
            } else {
                for cache in res.message {
                    println!("{}: {}", cache.name, cache.id);
                }
            }
        }

        Commands::Edit {
            name,
            display_name,
            description,
            priority,
        } => {
            let input_fields = [
                ("Display Name", display_name),
                ("Description", description),
                ("Priority", priority.map(|p| p.to_string())),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

            let input = handle_input(input_fields, false);

            let priority = match input.get("Priority").unwrap().parse::<i32>() {
                Ok(p) => p,
                Err(_) => {
                    eprintln!("Priority must be an integer.");
                    exit(1);
                }
            };

            let res = caches::put(
                get_request_config(load_config()).unwrap(),
                name.clone(),
                input.get("Display Name").unwrap().clone(),
                input.get("Description").unwrap().clone(),
                priority,
            )
            .await
            .map_err(|e| {
                eprintln!("{}", e);
                exit(1);
            })
            .unwrap();

            if res.error {
                eprintln!("Cache creation failed: {}", res.message);
                exit(1);
            }

            println!("Cache updated.");
        }

        Commands::Delete { name } => {
            let res =
                caches::delete_cache(get_request_config(load_config()).unwrap(), name.clone())
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

            if res.error {
                eprintln!("Cache deletion failed: {}", res.message);
                exit(1);
            }

            println!("Cache deleted.");
        }

        Commands::Show { name } => {
            let res =
                caches::get_cache_key(get_request_config(load_config()).unwrap(), name.clone())
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

            if res.error {
                eprintln!("Cache Key retrieval failed: {}", res.message);
                exit(1);
            }

            println!("Cache Public Key: {:?}", res.message);
        }

        Commands::InstallNetrc {
            server,
            token,
            cache,
            netrc_file,
        } => {
            let token = match token {
                Some(t) if !t.is_empty() => t,
                _ => {
                    print!("API key (GRAD…): ");
                    std::io::Write::flush(&mut std::io::stdout()).ok();
                    let entered = rpassword::read_password().unwrap_or_else(|e| {
                        eprintln!("Failed to read token: {}", e);
                        exit(1);
                    });
                    if entered.is_empty() {
                        eprintln!("Token cannot be empty.");
                        exit(1);
                    }
                    entered
                }
            };

            let machine_host = server
                .trim()
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .split('/')
                .next()
                .unwrap_or("")
                .to_string();
            if machine_host.is_empty() {
                eprintln!("Invalid server URL: '{}'", server);
                exit(1);
            }

            let new_entry = format!(
                "machine {}\nlogin gradient\npassword {}\n",
                machine_host, token
            );

            let existing = fs::read_to_string(&netrc_file).unwrap_or_default();
            let filtered = remove_netrc_entry(&existing, &machine_host);

            let updated = if filtered.ends_with('\n') || filtered.is_empty() {
                format!("{}{}", filtered, new_entry)
            } else {
                format!("{}\n{}", filtered, new_entry)
            };

            if let Some(parent) = std::path::Path::new(&netrc_file).parent()
                && let Err(e) = fs::create_dir_all(parent)
            {
                eprintln!("Failed to create directory '{}': {}", parent.display(), e);
                exit(1);
            }

            fs::write(&netrc_file, &updated).unwrap_or_else(|e| {
                eprintln!("Failed to write '{}': {}", netrc_file, e);
                exit(1);
            });

            println!(
                "netrc credentials for cache '{}' (machine '{}') installed into '{}'.",
                cache, machine_host, netrc_file
            );
        }
    }
}

/// Remove all lines belonging to a `machine <host>` block from a netrc file's contents.
/// A block ends at the next `machine` line or end of file.
fn remove_netrc_entry(contents: &str, host: &str) -> String {
    if host.is_empty() {
        return contents.to_string();
    }

    let mut result = String::new();
    let mut skip = false;

    for line in contents.lines() {
        if line.starts_with("machine ") {
            skip = line.split_whitespace().nth(1) == Some(host);
        }
        if !skip {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}
