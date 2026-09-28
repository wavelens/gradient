/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Export one evaluation as a SQLite file a maintainer can diagnose from.
//!
//! Building one reads every failed build's log, so it takes a logged-in caller
//! even where anonymous browsing is allowed. On top of that, reading the
//! evaluation costs `ViewProject` and instance context costs `ManageWorkers`,
//! refused rather than silently dropped, so the report's manifest can never
//! disagree with what was asked for.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use axum::{Extension, body::Body};
use gradient_core::ServerState;
use gradient_db::permissions::Permission;
use gradient_report::{ReportContext, ReportOptions, generate_report};
use gradient_types::MUser;
use gradient_types::ids::EvaluationId;
use serde::Deserialize;

use super::EvalAccessContext;
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};

#[derive(Deserialize)]
pub struct ReportQuery {
    anonymize_identities: Option<bool>,
    anonymize_packages: Option<bool>,
    include_logs: Option<bool>,
    include_instance: Option<bool>,
}

impl ReportQuery {
    /// Defaults hand over a report that names which package broke but not whose
    /// repository it is.
    fn options(&self) -> ReportOptions {
        ReportOptions {
            anonymize_identities: self.anonymize_identities.unwrap_or(true),
            anonymize_packages: self.anonymize_packages.unwrap_or(false),
            include_logs: self.include_logs.unwrap_or(true),
            include_instance: self.include_instance.unwrap_or(true),
        }
    }
}

/// The fleet and upstream sections describe more than the evaluation that asked
/// for them, so they cost the permission that already governs worker config.
pub(crate) fn report_requires_manage_workers(opts: &ReportOptions) -> bool {
    opts.include_instance
}

pub async fn get_evaluation_report(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
    Query(query): Query<ReportQuery>,
) -> WebResult<Response> {
    let user_id = user.id;
    let maybe_user = Some(user);
    let ctx = EvalAccessContext::load(&state, evaluation_id, &maybe_user, api_key.as_ref()).await?;
    let opts = query.options();

    if report_requires_manage_workers(&opts) {
        let allowed = crate::access::has_permission(
            &state,
            user_id,
            ctx.project_id,
            Permission::ManageWorkers,
            api_key.as_ref(),
        )
        .await?;

        if !allowed {
            return Err(WebError::forbidden(
                "Instance context requires the ManageWorkers permission",
            ));
        }
    }

    let file = tempfile::NamedTempFile::new()
        .map_err(|e| WebError::internal(format!("Failed to create report file: {e}")))?;

    let report_ctx = ReportContext {
        logs: state.log_storage.as_ref(),
        config: &state.config,
    };

    generate_report(
        &state.web_db,
        &report_ctx,
        evaluation_id.into_inner(),
        ctx.project_id.into_inner(),
        opts,
        file.path(),
    )
    .await
    .map_err(|e| WebError::internal(format!("Failed to generate report: {e:#}")))?;

    let bytes = std::fs::read(file.path())
        .map_err(|e| WebError::internal(format!("Failed to read report file: {e}")))?;

    let filename = report_filename(evaluation_id);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/vnd.sqlite3")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from(bytes))
        .map_err(|e| WebError::internal(format!("Failed to build response: {e}")))
}

fn report_filename(evaluation: EvaluationId) -> String {
    let id = evaluation.to_string();
    let short = id.split('-').next().unwrap_or(&id);
    format!(
        "gradient-report-{short}-{}.db",
        gradient_types::now().format("%Y-%m-%d")
    )
}
