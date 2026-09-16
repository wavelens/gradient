/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Explains every statement `gradient_db::sql!` registered, against the cache
//! VM's own database amplified to production scale, and fails on a pathological
//! plan. `docs/src/development/tests.md` documents where it runs and why.

mod amplify;
mod explain;
mod report;
mod sample;

use anyhow::Result;
use clap::Parser;
use sea_orm::Database;

#[derive(Parser)]
#[command(about = "Explain every registered SQL statement and fail on a bad plan")]
struct Cli {
    #[arg(long, env = "GRADIENT_DATABASE_URL")]
    database_url: String,
    /// Divides the amplification targets; 1 is the full production shape.
    #[arg(long, default_value_t = 1)]
    scale: u32,
    /// The gate fails when more queries than this cannot be measured.
    #[arg(long, default_value_t = 0)]
    max_unmeasured: usize,
    /// Measure the database as it stands, without amplifying it first.
    #[arg(long)]
    no_amplify: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let db = Database::connect(&cli.database_url).await?;

    if !cli.no_amplify {
        amplify::run(&db, cli.scale).await?;
    }

    let rows = explain::run_all(&db).await?;
    print!("{}", report::render(&rows));

    std::process::exit(report::exit_code(&rows, cli.max_unmeasured));
}
