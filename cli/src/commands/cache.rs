/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::cache_nar;
use crate::commands::cache_upload;
use crate::commands::completion;
use crate::input::{client_from_config, handle_input};
use crate::output::{ExitKind, Output, to_exit_kind};
use clap::Subcommand;
use clap_complete::engine::ArgValueCompleter;
use connector::caches::MakeCacheRequest;
use std::fs;

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
        /// Max cache storage in GB. 0 = unlimited (default); otherwise at least 1.
        #[arg(short = 'm', long, default_value_t = 0)]
        max_storage_gb: i32,
    },
    List,
    Edit {
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        name: String,
        #[arg(short, long)]
        display_name: Option<String>,
        #[arg(short = 'c', long)]
        description: Option<String>,
        #[arg(short, long)]
        priority: Option<i32>,
        /// Max cache storage in GB. 0 = unlimited (default); otherwise at least 1.
        #[arg(short = 'm', long, default_value_t = 0)]
        max_storage_gb: i32,
    },
    Delete {
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        name: String,
    },
    Show {
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        name: String,
    },
    /// Build a netrc entry locally for a cache and install it into a netrc file.
    /// No request is sent to the server - bring your own API key (issued via the
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
        #[arg(short, long, add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Path to the netrc file to update (default: /etc/nix/netrc)
        #[arg(short = 'f', long, default_value = "/etc/nix/netrc")]
        netrc_file: String,
    },
    /// Manage individual NARs in a cache
    Nar {
        #[command(subcommand)]
        cmd: cache_nar::Commands,
    },
    /// Upload NAR(s) to a cache
    Upload(crate::commands::cache_upload::UploadArgs),
}

pub async fn handle(cmd: Commands, out: Output) {
    match cmd {
        Commands::Create {
            name,
            display_name,
            description,
            priority,
            max_storage_gb,
        } => {
            let input_fields = [
                ("Name", name),
                ("Display Name", display_name),
                ("Description", description),
                ("Priority", priority.map(|p| p.to_string())),
                ("Max Storage (GB)", Some(max_storage_gb.to_string())),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

            let input = handle_input(input_fields, true);
            let name = input.get("Name").unwrap().clone();

            let priority = match input.get("Priority").unwrap().parse::<i32>() {
                Ok(p) => p,
                Err(_) => out.err(ExitKind::Usage, "Priority must be an integer."),
            };
            let max_storage_gb = parse_max_storage_gb(input.get("Max Storage (GB)").unwrap(), out);

            let client = client_from_config(out);
            match client
                .caches()
                .create(MakeCacheRequest {
                    name,
                    display_name: input.get("Display Name").unwrap().clone(),
                    description: input.get("Description").unwrap().clone(),
                    priority,
                    max_storage_gb,
                })
                .await
            {
                Ok(_) => {
                    out.ok(&serde_json::json!({"created": true}));
                    out.human("Cache created.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::List => {
            let client = client_from_config(out);
            match client.caches().list().await {
                Ok(caches) => {
                    out.ok(&caches);
                    if caches.is_empty() {
                        out.human("You have no caches.");
                    } else {
                        for cache in caches {
                            out.human(format!("{}: {}", cache.name, cache.id));
                        }
                    }
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::Edit {
            name,
            display_name,
            description,
            priority,
            max_storage_gb,
        } => {
            let input_fields = [
                ("Display Name", display_name),
                ("Description", description),
                ("Priority", priority.map(|p| p.to_string())),
                ("Max Storage (GB)", Some(max_storage_gb.to_string())),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();

            let input = handle_input(input_fields, false);

            let priority = match input.get("Priority").unwrap().parse::<i32>() {
                Ok(p) => p,
                Err(_) => out.err(ExitKind::Usage, "Priority must be an integer."),
            };
            let max_storage_gb = parse_max_storage_gb(input.get("Max Storage (GB)").unwrap(), out);

            let client = client_from_config(out);
            match client
                .caches()
                .create(MakeCacheRequest {
                    name,
                    display_name: input.get("Display Name").unwrap().clone(),
                    description: input.get("Description").unwrap().clone(),
                    priority,
                    max_storage_gb,
                })
                .await
            {
                Ok(_) => {
                    out.ok(&serde_json::json!({"updated": true}));
                    out.human("Cache updated.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::Delete { name } => {
            let client = client_from_config(out);
            match client.caches().delete(&name).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"deleted": true}));
                    out.human("Cache deleted.");
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
        }

        Commands::Show { name } => {
            let client = client_from_config(out);
            match client.caches().public_key(&name).await {
                Ok(key) => {
                    out.ok(&serde_json::json!({"public_key": key}));
                    out.human(format!("Cache Public Key: {}", key));
                }
                Err(e) => out.err(to_exit_kind(&e), e),
            }
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
                        out.err(ExitKind::Api, format!("Failed to read token: {}", e));
                    });
                    if entered.is_empty() {
                        out.err(ExitKind::Usage, "Token cannot be empty.");
                    }
                    entered
                }
            };

            let machine_host = crate::netrc::machine_host(&server);
            if machine_host.is_empty() {
                out.err(ExitKind::Usage, format!("Invalid server URL: '{}'", server));
            }

            let new_entry = crate::netrc::entry(&machine_host, &token);

            let existing = fs::read_to_string(&netrc_file).unwrap_or_default();
            let filtered = crate::netrc::remove_entry(&existing, &machine_host);

            let updated = if filtered.ends_with('\n') || filtered.is_empty() {
                format!("{}{}", filtered, new_entry)
            } else {
                format!("{}\n{}", filtered, new_entry)
            };

            if let Some(parent) = std::path::Path::new(&netrc_file).parent()
                && let Err(e) = fs::create_dir_all(parent)
            {
                out.err(
                    ExitKind::Api,
                    format!("Failed to create directory '{}': {}", parent.display(), e),
                );
            }

            fs::write(&netrc_file, &updated).unwrap_or_else(|e| {
                out.err(
                    ExitKind::Api,
                    format!("Failed to write '{}': {}", netrc_file, e),
                );
            });

            out.human(format!(
                "netrc credentials for cache '{}' (machine '{}') installed into '{}'.",
                cache, machine_host, netrc_file
            ));
        }

        Commands::Nar { cmd } => cache_nar::handle(cmd, out).await,
        Commands::Upload(args) => cache_upload::handle(args, out).await,
    }
}

fn parse_max_storage_gb(raw: &str, out: Output) -> i32 {
    match raw.trim().parse::<i32>() {
        Ok(v) if v >= 0 => v,
        Ok(_) => out.err(
            ExitKind::Usage,
            "Max storage must be 0 (unlimited) or at least 1.",
        ),
        Err(_) => out.err(ExitKind::Usage, "Max storage must be an integer."),
    }
}

