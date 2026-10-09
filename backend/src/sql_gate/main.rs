/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod amplify;
mod explain;
mod report;
mod sample;
mod unanalyzed;

use anyhow::{Context, Result};
use clap::Parser;
use gradient_db::sql::{Param, registry};
use sample::Sampler;
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Value};

/// A linker is dropping an rlib the binary never mentions. The crate's registry entries would be
/// silently absent without these calls, and the gate would measure a subset.
const LINKED: &[(&str, fn())] = &[
    ("gradient-cache", gradient_cache::link),
    ("gradient-ci", gradient_ci::link),
    ("gradient-db", || {}),
    ("gradient-graph", gradient_graph::link),
    ("gradient-proto", gradient_proto::link),
    ("gradient-report", gradient_report::link),
    ("gradient-scheduler", gradient_scheduler::link),
    ("gradient-web", gradient_web::link),
];

#[derive(Parser)]
#[command(about = "Explain every registered SQL statement and fail on a bad plan")]
struct Cli {
    #[arg(long, env = "GRADIENT_DATABASE_URL")]
    database_url: Option<String>,
    /// Print the registry and exit, without touching a database.
    #[arg(long)]
    list: bool,
    /// Print one registered statement's text and exit. Tests are using it to run the exact server
    /// SQL.
    #[arg(long, value_name = "NAME")]
    print: Option<String>,
    /// Divisor for the amplification targets. 1 is the full production shape.
    #[arg(long, default_value_t = 1)]
    scale: u32,
    /// Maximum number of unmeasurable queries the gate is accepting.
    #[arg(long, default_value_t = 0)]
    max_unmeasured: usize,
    /// Measure the database as it stands, without amplifying it first.
    #[arg(long)]
    no_amplify: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    for (_, link) in LINKED {
        link();
    }

    missing_crates()
        .is_empty()
        .then_some(())
        .context("a crate's statements did not reach the registry")?;

    if cli.list {
        list();
        return Ok(());
    }

    if let Some(name) = &cli.print {
        println!(
            "{}",
            statement_text(name).context("no registered statement by that name")?
        );
        return Ok(());
    }

    let url = cli
        .database_url
        .context("--database-url or GRADIENT_DATABASE_URL is required")?;
    let db = Database::connect(&url).await?;

    if !cli.no_amplify {
        amplify::run(&db, cli.scale).await?;
    }

    // Production's autovacuum is keeping the visibility map set. A freshly amplified database has
    // none, and every index-only scan would read the heap.
    db.execute_unprepared("VACUUM (ANALYZE)")
        .await
        .context("VACUUM (ANALYZE) before measuring")?;

    let mut sampler = Sampler::default();
    if let Some(evaluation) = unanalyzed::copy_largest_evaluation(&db).await? {
        sampler.pin(&Param::EvaluationId, Value::from(evaluation));
    }

    let rows = explain::run_all(&bounded(&url).await?, sampler).await?;
    print!("{}", report::render(&rows));

    std::process::exit(report::exit_code(&rows, cli.max_unmeasured));
}

async fn bounded(url: &str) -> Result<DatabaseConnection> {
    let mut options = ConnectOptions::new(url);
    options.map_sqlx_postgres_opts(|pg| {
        pg.options([("statement_timeout", "30s"), ("lock_timeout", "2s")])
    });

    Ok(Database::connect(options).await?)
}

fn missing_crates() -> Vec<&'static str> {
    let files: Vec<&str> = registry().map(|query| query.file).collect();

    LINKED
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !files.iter().any(|file| file.starts_with(name)))
        .collect()
}

fn statement_text(name: &str) -> Option<String> {
    registry()
        .find(|query| query.name == name)
        .map(|query| query.text().into_owned())
}

fn list() {
    let mut queries: Vec<_> = registry().collect();
    queries.sort_by_key(|query| (query.file, query.line));

    for query in &queries {
        println!(
            "{:<28} {:<26} {:?} params={} {}",
            query.name,
            query.location(),
            query.tier,
            query.params.len(),
            query.file,
        );
    }

    println!("{} registered statements", queries.len());
}

#[cfg(test)]
mod tests {
    use super::statement_text;

    #[test]
    fn print_answers_a_registered_name_with_its_text_and_nothing_else() {
        for (_, link) in super::LINKED {
            link();
        }
        let text = statement_text("LOCK_SEED_SHARED_BUILDS").expect("registered");
        assert!(text.contains("pg_advisory_xact_lock_shared"), "{text}");
        assert!(statement_text("NO_SUCH_STATEMENT").is_none());
    }
}
