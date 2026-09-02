/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Snapshot one evaluation, the instance context that explains it, and its
//! failed-build logs into a SQLite file a maintainer can diagnose from without
//! access to the instance.
//!
//! Fetching from Postgres and redacting-and-writing are deliberately separate:
//! the redaction path takes rows directly, so what the file ends up containing
//! is testable without a database.

mod config_snapshot;
mod extract;
mod guarantee;
mod logs;
mod redact;
mod schema;
mod tables;

pub use config_snapshot::write_config_snapshot;
pub use extract::{create_table, export_tables, fetch_rows, manifest_row, redact_row, write_rows};
pub use logs::{FetchedLogs, create_log_table, fetch_failed_logs, insert_log, write_failed_logs};
pub use redact::Redactor;
pub use schema::{
    ManifestRow, ReportOptions, SCHEMA_VERSION, open_report, write_manifest, write_meta,
};
pub use tables::{Row, TableSpec, eval_scope_tables, instance_tables, redact_value};

use std::path::{Path, PathBuf};

use anyhow::Result;
use gradient_storage::LogStorage;
use gradient_types::{EvalArgs, ProtoArgs, S3Config, StorageArgs};
use sea_orm::ConnectionTrait;

/// Everything the generator needs that is not the database or the options.
pub struct ReportContext<'a> {
    pub logs: &'a dyn LogStorage,
    pub eval_args: &'a EvalArgs,
    pub proto_args: &'a ProtoArgs,
    pub storage_args: &'a StorageArgs,
    pub s3_config: Option<&'a S3Config>,
}

/// Write one evaluation's report to `out`.
///
/// Fetching runs async and writing runs on a blocking thread: a `rusqlite`
/// connection is not `Send`, so holding one across an await would make the whole
/// handler future non-`Send`. Splitting the phases is what the pure `write_rows`
/// half was for.
///
/// `evaluation` and `project` are strings because every exported query casts to
/// text; they are the only two scopes the specs take.
pub async fn generate_report<C: ConnectionTrait>(
    db: &C,
    ctx: &ReportContext<'_>,
    evaluation: &str,
    project: &str,
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
    let evaluation = evaluation.to_owned();
    let eval_args = ctx.eval_args.clone();
    let proto_args = ctx.proto_args.clone();
    let storage_args = ctx.storage_args.clone();
    let s3_config = ctx.s3_config.cloned();

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
            write_config_snapshot(
                &conn,
                &eval_args,
                &proto_args,
                &storage_args,
                s3_config.as_ref(),
            )?;
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
