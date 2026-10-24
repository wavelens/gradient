/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod document;

use anyhow::Context;
use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::Response;
use gradient_db::graph::runtime_closure::runtime_closure_reachable;
use gradient_db::graph::sbom::{derivation_names, entry_point_outputs, with_referenced_paths};
use gradient_db::lookup::by_id;
use gradient_types::input::vec_to_hex;
use gradient_types::*;

use self::document::{SbomInput, component_hashes, cyclonedx, with_original_hashes};
use crate::api_spec::{Binary, TaskPath};
use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use crate::resource::Resource;
use crate::scope::{Readable, TaskScope};

#[utoipa::path(
    get,
    path = "/tasks/{project}/{task}/sbom",
    operation_id = "getTaskSbom",
    tag = "tasks",
    summary = "Download the task's SBOM",
    description = "CycloneDX 1.6 JSON of the runtime closure of the entry point outputs of the\ntask's latest completed evaluation. Licenses come from `meta.license` and are\nknown for entry points only. Paths missing from the cache are listed without\nhashes or dependencies and the document is then marked `incomplete`.\nReturns `404` when the task has no completed evaluation.",
    params(TaskPath),
    responses((status = OK, description = "CycloneDX SBOM", body = Binary, content_type = "application/vnd.cyclonedx+json"), WebError)
)]
pub async fn get_task_sbom(
    TaskScope {
        project, task, db, ..
    }: TaskScope<Readable>,
) -> WebResult<Response> {
    let evaluation = gradient_db::evaluations::latest_completed_of_task(&db.web_db, task.id, 1)
        .await?
        .into_iter()
        .next()
        .or_not_found(Resource::Evaluation)?;

    let commit = by_id::<ECommit, _>(&db.web_db, evaluation.commit)
        .await?
        .map(|c| vec_to_hex(&c.hash));
    let entry_points = entry_point_outputs(&db.web_db, evaluation.id).await?;
    let seeds: Vec<String> = entry_points.iter().map(|ep| ep.hash.clone()).collect();
    let reached = runtime_closure_reachable(&db.web_db, &seeds).await?;
    let reached = with_referenced_paths(&db.web_db, reached).await?;
    let names = derivation_names(&db.web_db, &component_hashes(&entry_points, &reached)).await?;

    let input = SbomInput {
        task: format!("{}/{}", project.name, task.name),
        evaluation: evaluation.id,
        commit,
        created_at: evaluation.created_at,
        entry_points,
        reached,
        names,
    };
    let mapper = db.project_mapper(project.id).await?;
    let body = serde_json::to_vec_pretty(&cyclonedx(&with_original_hashes(input, &mapper)))
        .context("serialize sbom")?;

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/vnd.cyclonedx+json")
        .header(
            header::CONTENT_DISPOSITION,
            format!(
                "attachment; filename=\"{}-{}-sbom.cdx.json\"",
                project.name, task.name
            ),
        )
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from(body))
        .context("build response")
        .map_err(Into::<WebError>::into)
}
