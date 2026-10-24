/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod document;

use anyhow::Context;
use axum::Extension;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use gradient_core::ServerState;
use gradient_db::graph::runtime_closure::runtime_closure_reachable;
use gradient_db::graph::sbom::{build_outputs, derivation_names, with_referenced_paths};
use gradient_db::lookup::by_id;
use gradient_types::input::vec_to_hex;
use gradient_types::*;
use std::sync::Arc;

use self::document::{SbomInput, component_hashes, cyclonedx, with_original_hashes};
use super::BuildAccessContext;
use crate::api_spec::Binary;
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::lookup::require;

#[utoipa::path(
    get,
    path = "/builds/{build}/sbom",
    operation_id = "getBuildSbom",
    tag = "builds",
    summary = "Download the build's SBOM",
    description = "CycloneDX 1.6 JSON of the runtime closure of the build's outputs. Licenses come\nfrom `meta.license` and are known for entry points only. Paths missing from the\ncache are listed without hashes or dependencies and the document is then\nmarked `incomplete`.",
    params(("build" = BuildJobId, Path)),
    responses((status = OK, description = "CycloneDX SBOM", body = Binary, content_type = "application/vnd.cyclonedx+json"), WebError)
)]
pub async fn get_build_sbom(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Response> {
    let ctx =
        BuildAccessContext::load(&state.web_db, build_id, &maybe_user, api_key.as_ref()).await?;
    let job = &ctx.build_job;
    let evaluation = require::<EEvaluation>(&state.web_db, job.evaluation).await?;
    let name = by_id::<EDerivation, _>(&state.web_db, job.derivation)
        .await?
        .map_or_else(|| build_id.to_string(), |d| d.name);
    let commit = by_id::<ECommit, _>(&state.web_db, evaluation.commit)
        .await?
        .map(|c| vec_to_hex(&c.hash));

    let roots = build_outputs(&state.web_db, evaluation.id, job.derivation).await?;
    let seeds: Vec<String> = roots.iter().map(|root| root.hash.clone()).collect();
    let reached = runtime_closure_reachable(&state.web_db, &seeds).await?;
    let reached = with_referenced_paths(&state.web_db, reached).await?;
    let names = derivation_names(&state.web_db, &component_hashes(&roots, &reached)).await?;

    let input = SbomInput {
        subject: name.clone(),
        serial: build_id,
        commit,
        created_at: evaluation.created_at,
        roots,
        reached,
        names,
    };
    let mapper = state.project_mapper(ctx.project.id).await?;
    let body = serde_json::to_vec_pretty(&cyclonedx(&with_original_hashes(input, &mapper)))
        .context("serialize sbom")?;

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/vnd.cyclonedx+json")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{name}-sbom.cdx.json\""),
        )
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from(body))
        .context("build response")
        .map_err(Into::<WebError>::into)
}
