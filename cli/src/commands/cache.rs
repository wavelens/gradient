/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::*;
use crate::input::*;
use clap::{Subcommand, arg};
use connector::*;
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
    }
}
