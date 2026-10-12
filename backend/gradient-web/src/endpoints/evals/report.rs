/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Building a report is reading every failed build's log. It is requiring a logged-in caller even
//! where anonymous browsing is allowed.

use crate::api_spec::Binary;
use anyhow::Context;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use axum::{Extension, body::Body};
use gradient_core::ServerState;
use gradient_report::{ReportContext, ReportOptions, generate_report};
use gradient_types::MUser;
use gradient_types::ids::EvaluationId;
use gradient_types::permissions::Permission;
use serde::Deserialize;

use super::EvalAccessContext;
use super::refusal::EvaluationRefusal;
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ReportQuery {
    anonymize_identities: Option<bool>,
    anonymize_packages: Option<bool>,
    include_logs: Option<bool>,
    include_instance: Option<bool>,
}

impl ReportQuery {
    fn options(&self) -> ReportOptions {
        ReportOptions {
            anonymize_identities: self.anonymize_identities.unwrap_or(true),
            anonymize_packages: self.anonymize_packages.unwrap_or(false),
            include_logs: self.include_logs.unwrap_or(true),
            include_instance: self.include_instance.unwrap_or(true),
        }
    }
}

/// The fleet and upstream sections are describing more than the evaluation. They require the
/// permission that is already governing worker config.
pub(crate) fn report_requires_manage_workers(opts: &ReportOptions) -> bool {
    opts.include_instance
}

#[utoipa::path(
    get,
    path = "/evals/{evaluation}/report",
    operation_id = "getEvaluationReport",
    tag = "evals",
    summary = "Download an evaluation diagnostic report",
    description = "Exports the evaluation, the instance context that explains it, and its\nfailed-build logs as a SQLite database, for attaching to a bug report.\nOpen it with any SQLite client, or with the `gradient-report` inspector.\n\nThe export carries the direct dependencies of the evaluation's\nderivations, build-time and runtime alike, and their outputs' paths, so\n`blocking_deps` and `missing_runtime_deps` can be recomputed from the file\nalone and an absent row means the instance never had it.\n\nRequires authentication even for a public project, whose evaluations are\notherwise browsable anonymously: a report reads every failed build's log\nin one request. Reading the evaluation requires `ViewProject`.\n`include_instance` also requires `ManageWorkers`, since the fleet and\nupstream sections describe more than the evaluation that asked for them;\nit is refused rather than silently dropped, so the report's manifest\nalways matches the request.\n\nAnonymisation replaces identifying strings with tokens derived from a\nper-report salt, stable within one file and uncorrelatable across two.\nBuild logs are rewritten against those same tokens, plus any store path\nthey name, so a log cannot reintroduce what the columns redacted.\nNix store hashes are always preserved: they are one-way, and they are\nwhat makes an upstream cache check possible.\n\nThe parameter defaults below apply to callers that omit them. The web\ndialog asks the inverse question - what to *include* - and always sends\nall four explicitly, with build logs off unless asked for.",
    params(("evaluation" = EvaluationId, Path), ReportQuery),
    responses((status = OK, description = "SQLite report", body = Binary, content_type = "application/vnd.sqlite3"), WebError)
)]
pub async fn get_evaluation_report(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
    Query(query): Query<ReportQuery>,
) -> WebResult<Response> {
    let user_id = user.id;
    let maybe_user = Some(user);
    let ctx = EvalAccessContext::load(&state.web_db, evaluation_id, &maybe_user, api_key.as_ref())
        .await?;
    let opts = query.options();

    if report_requires_manage_workers(&opts) {
        let allowed = crate::access::has_permission(
            &state.web_db,
            user_id,
            ctx.project_id,
            Permission::ManageWorkers,
            api_key.as_ref(),
        )
        .await?;

        if !allowed {
            return Err(EvaluationRefusal::ManageWorkersRequired.into());
        }
    }

    let file = tempfile::NamedTempFile::new().context("create report file")?;

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
    .context("generate report")?;

    let bytes = std::fs::read(file.path()).context("read report file")?;

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
        .context("build response")
        .map_err(Into::into)
}

fn report_filename(evaluation: EvaluationId) -> String {
    let id = evaluation.to_string();
    let short = id.split('-').next().unwrap_or(&id);
    format!(
        "gradient-report-{short}-{}.db",
        gradient_types::now().format("%Y-%m-%d")
    )
}
