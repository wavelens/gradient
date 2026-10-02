/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod config_snapshot;
mod extract;
mod guarantee;
mod logs;
mod redact;
mod schema;
mod tables;

pub use config_snapshot::write_config_snapshot;
pub use extract::{
    create_table, export_tables, fetch_rows, manifest_row, redact_row, scope_statement, write_rows,
};
pub use logs::{FetchedLogs, create_log_table, fetch_failed_logs, insert_log, write_failed_logs};
pub use redact::Redactor;
pub use schema::{
    ManifestRow, ReportOptions, SCHEMA_VERSION, open_report, write_manifest, write_meta,
};
pub use tables::{Row, TableSpec, eval_scope_tables, instance_tables, redact_value};

use std::path::{Path, PathBuf};

use anyhow::Result;
use gradient_storage::LogStorage;
use gradient_types::RuntimeConfig;
use sea_orm::ConnectionTrait;
use sea_orm::prelude::Uuid;

pub struct ReportContext<'a> {
    pub logs: &'a dyn LogStorage,
    pub config: &'a RuntimeConfig,
}

/// Fetching is async and writing is running on a blocking thread. A `rusqlite` connection is not
/// `Send`, and holding one across an await would make the handler future non-`Send`.
pub async fn generate_report<C: ConnectionTrait>(
    db: &C,
    ctx: &ReportContext<'_>,
    evaluation: Uuid,
    project: Uuid,
    opts: ReportOptions,
    out: &Path,
) -> Result<()> {
    let mut fetched: Vec<(&'static TableSpec, Vec<Row>)> = Vec::new();
    for spec in eval_scope_tables() {
        fetched.push((spec, fetch_rows(db, spec, evaluation).await?));
    }

    if opts.include_instance {
        for spec in instance_tables() {
            fetched.push((spec, fetch_rows(db, spec, project).await?));
        }
    }

    let logs = if opts.include_logs {
        Some(fetch_failed_logs(db, ctx.logs, evaluation).await?)
    } else {
        None
    };

    let path: PathBuf = out.to_path_buf();
    let evaluation = evaluation.to_string();
    let config = ctx.config.clone();

    tokio::task::spawn_blocking(move || {
        let conn = open_report(&path)?;
        write_meta(&conn, &evaluation, &opts)?;

        let redactor = Redactor::new(opts);
        let mut manifest = Vec::with_capacity(fetched.len() + 1);
        for (spec, rows) in &fetched {
            create_table(&conn, spec)?;
            let redacted: Vec<Row> = rows
                .iter()
                .map(|r| redact_row(spec, &redactor, r))
                .collect();
            write_rows(&conn, spec, &redacted)?;
            manifest.push(manifest_row(spec, &redactor, redacted.len() as i64));
        }

        if opts.include_instance {
            write_config_snapshot(&conn, &config)?;
        }

        if let Some(logs) = logs {
            manifest.push(write_failed_logs(&conn, &redactor, &logs)?);
        }

        write_manifest(&conn, &manifest)?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("report writer panicked: {e}"))?
}

/// A linker is dropping an rlib nothing mentions, registry entries included. Calling this is
/// pulling the `gradient_db::sql!` statements into the plan gate registry.
pub const fn link() {}
