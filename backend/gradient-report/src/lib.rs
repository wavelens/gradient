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
mod logs;
mod redact;
mod schema;
mod tables;

pub use config_snapshot::write_config_snapshot;
pub use extract::{create_table, export_tables, fetch_rows, redact_row, write_rows};
pub use logs::{create_log_table, export_failed_logs, insert_log};
pub use redact::Redactor;
pub use schema::{
    ManifestRow, ReportOptions, SCHEMA_VERSION, open_report, write_manifest, write_meta,
};
pub use tables::{Row, TableSpec, eval_scope_tables, instance_tables, redact_value};

use std::path::Path;

use anyhow::Result;
use gradient_storage::LogStorage;
use gradient_types::{EvalArgs, ProtoArgs, S3Args, StorageArgs};
use sea_orm::ConnectionTrait;

/// Everything the generator needs that is not the database or the options.
pub struct ReportContext<'a> {
    pub logs: &'a dyn LogStorage,
    pub eval_args: &'a EvalArgs,
    pub proto_args: &'a ProtoArgs,
    pub storage_args: &'a StorageArgs,
    pub s3_args: &'a S3Args,
}

/// Write one evaluation's report to `out`.
///
/// `evaluation` and `project` are passed as strings because every exported
/// query casts to text; they are the only two scopes the specs take.
pub async fn generate_report<C: ConnectionTrait>(
    db: &C,
    ctx: &ReportContext<'_>,
    evaluation: &str,
    project: &str,
    opts: ReportOptions,
    out: &Path,
) -> Result<()> {
    let conn = open_report(out)?;
    write_meta(&conn, evaluation, &opts)?;

    let redactor = Redactor::new(opts);
    let mut manifest = export_tables(db, &conn, eval_scope_tables(), evaluation, &redactor).await?;

    if opts.include_instance {
        manifest.extend(export_tables(db, &conn, instance_tables(), project, &redactor).await?);
        write_config_snapshot(
            &conn,
            ctx.eval_args,
            ctx.proto_args,
            ctx.storage_args,
            ctx.s3_args,
        )?;
    }

    if opts.include_logs {
        manifest.push(export_failed_logs(db, &conn, ctx.logs, evaluation).await?);
    }

    write_manifest(&conn, &manifest)?;
    Ok(())
}
