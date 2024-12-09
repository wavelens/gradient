/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */

pub mod consts;
pub mod database;
pub mod executer;
pub mod input;
pub mod sources;
pub mod types;

use clap::Parser;
use database::connect_db;
use std::sync::Arc;
use types::*;

pub async fn init_state() -> Arc<ServerState> {
    let cli = Cli::parse();

    println!("Starting Gradient Server on {}:{}", cli.ip, cli.port);

    let db = connect_db(&cli).await;

    Arc::new(ServerState { db, cli })
}
