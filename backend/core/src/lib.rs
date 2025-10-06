/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod consts;
pub mod database;
pub mod email;
pub mod executer;
pub mod input;
pub mod permission;
pub mod sources;
pub mod state;
pub mod types;

use clap::Parser;
use database::connect_db;
use state::load_and_apply_state;
use std::sync::Arc;
use types::*;

pub async fn init_state() -> Arc<ServerState> {
    let cli = Cli::parse();

    println!("Starting Gradient Server on {}:{}", cli.ip, cli.port);
    println!("State file configured: {:?}", cli.state_file);

    let db = connect_db(&cli).await;

    // Load and apply state configuration if provided
    if let Err(e) =
        load_and_apply_state(&db, cli.state_file.as_deref(), &cli.crypt_secret_file).await
    {
        eprintln!("Failed to load state configuration: {}", e);
        std::process::exit(1);
    }

    Arc::new(ServerState { db, cli })
}
