/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, ProjectItemAccessRequirement};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::extractors::Path;
use crate::helpers::ok_json;
use crate::store_paths::original_text;
use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_types::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::EvalAccessContext;
pub use gradient_api_types::{ArtefactTree, EntryPointArtefacts, OutputArtefacts, ProductArtefact};

fn build_artefact_tree_from_evaluation_row_and_entry_point_artefacts(
    evaluation: &MEvaluation,
    entry_points: Vec<EntryPointArtefacts>,
) -> ArtefactTree {
    ArtefactTree {
        evaluation: evaluation.id,
        created_at: evaluation.created_at,
        entry_points,
    }
}

fn build_output_artefacts_from_derivation_output(
    mapper: &StorePathMapper,
    output: MDerivationOutput,
    products: Vec<ProductArtefact>,
) -> OutputArtefacts {
    OutputArtefacts {
        store_path: output
            .store_path()
            .map(|p| original_text(mapper, p.base()))
            .unwrap_or_default(),
        name: output.name,
        products,
    }
}

#[utoipa::path(
    get,
    path = "/evals/{evaluation}/artefacts",
    operation_id = "listEvaluationArtefacts",
    tag = "evals",
    summary = "List evaluation artefacts",
    description = "Returns the artefact tree for an evaluation: entry points grouped by\nderivation, derivation outputs grouped under each entry point, and\n`build_product` rows grouped under each output. Consumed by the CLI's\n`gradient download` picker so users can choose what to fetch without\nfirst listing builds.\n\nEntry points are sorted by `attr`, outputs by `name`, products by `path`.",
    params(("evaluation" = EvaluationId, Path)),
    responses((status = OK, description = "Artefact tree", body = ArtefactTree), WebError)
)]
pub async fn get_artefacts(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<ArtefactTree>>> {
    let ctx = EvalAccessContext::load(
        &state.web_db,
        evaluation_id,
        Caller::from(&maybe_user),
        api_key.as_ref(),
        ProjectItemAccessRequirement::PublicProjectOrCanViewProject,
    )
    .await?;
    let evaluation = ctx.evaluation;
    let mapper = state.project_mapper(ctx.project_id).await?;

    let entry_points =
        gradient_db::entry_points::of_evaluation(&state.web_db, evaluation.id).await?;

    if entry_points.is_empty() {
        return Ok(ok_json(
            build_artefact_tree_from_evaluation_row_and_entry_point_artefacts(&evaluation, vec![]),
        ));
    }

    let drv_ids: Vec<DerivationId> = entry_points
        .iter()
        .map(|ep| ep.derivation)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let build_job_by_drv: HashMap<DerivationId, BuildJobId> =
        gradient_db::build_jobs::of_derivations_in_evaluation(
            &state.web_db,
            evaluation.id,
            &drv_ids,
        )
        .await?
        .into_iter()
        .map(|row| (row.derivation, row.id))
        .collect();

    let derivations =
        gradient_db::graph::reachability::derivations_by_id(&state.web_db, &drv_ids).await?;

    let mut outputs_by_drv: HashMap<DerivationId, Vec<MDerivationOutput>> = HashMap::new();
    for row in gradient_db::derivation_outputs::of_derivations(&state.web_db, &drv_ids).await? {
        outputs_by_drv.entry(row.derivation).or_default().push(row);
    }

    let output_ids: Vec<DerivationOutputId> = outputs_by_drv
        .values()
        .flat_map(|v| v.iter().map(|o| o.id))
        .collect();

    let mut products_by_output: HashMap<DerivationOutputId, Vec<MBuildProduct>> = HashMap::new();
    for row in gradient_db::build_products::of_outputs(&state.web_db, &output_ids).await? {
        products_by_output
            .entry(row.derivation_output)
            .or_default()
            .push(row);
    }

    let mut tree: Vec<EntryPointArtefacts> = entry_points
        .into_iter()
        .filter_map(|ep| {
            let build_id = *build_job_by_drv.get(&ep.derivation)?;
            let drv = derivations.get(&ep.derivation)?;
            let outputs = outputs_by_drv
                .get(&drv.id)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|o| {
                    let products = products_by_output
                        .get(&o.id)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|p| ProductArtefact {
                            path: original_text(&mapper, p.path.clone()),
                            ..ProductArtefact::from(p)
                        })
                        .collect();
                    build_output_artefacts_from_derivation_output(&mapper, o, products)
                })
                .collect();
            Some(EntryPointArtefacts {
                attr: ep.eval,
                derivation: original_text(&mapper, drv.drv_path()),
                build_id,
                outputs,
            })
        })
        .collect();

    tree.sort_by(|a, b| a.attr.cmp(&b.attr));
    for ep in &mut tree {
        ep.outputs.sort_by(|a, b| a.name.cmp(&b.name));
        for o in &mut ep.outputs {
            o.products.sort_by(|a, b| a.path.cmp(&b.path));
        }
    }

    Ok(ok_json(
        build_artefact_tree_from_evaluation_row_and_entry_point_artefacts(&evaluation, tree),
    ))
}
