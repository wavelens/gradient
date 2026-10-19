/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::store_paths::original_text;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_types::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::EvalAccessContext;

const IS_IN_CHUNK: usize = 10_000;

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct ArtefactTree {
    pub evaluation: EvaluationId,
    pub created_at: chrono::NaiveDateTime,
    pub entry_points: Vec<EntryPointArtefacts>,
}

impl ArtefactTree {
    fn new(evaluation: &MEvaluation, entry_points: Vec<EntryPointArtefacts>) -> Self {
        Self {
            evaluation: evaluation.id,
            created_at: evaluation.created_at,
            entry_points,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct EntryPointArtefacts {
    pub attr: String,
    pub derivation: String,
    pub build_id: BuildJobId,
    pub outputs: Vec<OutputArtefacts>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct OutputArtefacts {
    pub name: String,
    pub store_path: String,
    pub products: Vec<ProductArtefact>,
}

impl OutputArtefacts {
    fn new(
        mapper: &StorePathMapper,
        output: MDerivationOutput,
        products: Vec<ProductArtefact>,
    ) -> Self {
        Self {
            store_path: original_text(mapper, StorePath::from(output.clone()).base()),
            name: output.name,
            products,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct ProductArtefact {
    pub id: BuildProductId,
    #[serde(rename = "type")]
    pub file_type: String,
    pub subtype: String,
    pub name: String,
    pub path: String,
    pub size: Option<i64>,
}

impl From<MBuildProduct> for ProductArtefact {
    fn from(p: MBuildProduct) -> Self {
        Self {
            id: p.id,
            file_type: p.file_type,
            subtype: p.subtype,
            name: p.name,
            path: p.path,
            size: p.size,
        }
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
    let ctx = EvalAccessContext::load(&state.web_db, evaluation_id, &maybe_user, api_key.as_ref())
        .await?;
    let evaluation = ctx.evaluation;
    let mapper = state.store_path_mapper(MappingContext::Project(ctx.project_id));

    let entry_points =
        gradient_db::entry_points::of_evaluation(&state.web_db, evaluation.id).await?;

    if entry_points.is_empty() {
        return Ok(ok_json(ArtefactTree::new(&evaluation, vec![])));
    }

    let drv_ids: Vec<DerivationId> = entry_points
        .iter()
        .map(|ep| ep.derivation)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let mut build_job_by_drv: HashMap<DerivationId, BuildJobId> = HashMap::new();
    for chunk in drv_ids.chunks(IS_IN_CHUNK) {
        for row in gradient_db::build_jobs::of_derivations_in_evaluation(
            &state.web_db,
            evaluation.id,
            chunk,
        )
        .await?
        {
            build_job_by_drv.insert(row.derivation, row.id);
        }
    }

    let derivations =
        gradient_db::graph::reachability::derivations_by_id(&state.web_db, &drv_ids).await?;

    let mut outputs_by_drv: HashMap<DerivationId, Vec<MDerivationOutput>> = HashMap::new();
    if !drv_ids.is_empty() {
        for chunk in drv_ids.chunks(IS_IN_CHUNK) {
            for row in gradient_db::derivation_outputs::of_derivations(&state.web_db, chunk).await?
            {
                outputs_by_drv.entry(row.derivation).or_default().push(row);
            }
        }
    }

    let output_ids: Vec<DerivationOutputId> = outputs_by_drv
        .values()
        .flat_map(|v| v.iter().map(|o| o.id))
        .collect();

    let mut products_by_output: HashMap<DerivationOutputId, Vec<MBuildProduct>> = HashMap::new();
    if !output_ids.is_empty() {
        for chunk in output_ids.chunks(IS_IN_CHUNK) {
            for row in gradient_db::build_products::of_outputs(&state.web_db, chunk).await? {
                products_by_output
                    .entry(row.derivation_output)
                    .or_default()
                    .push(row);
            }
        }
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
                    OutputArtefacts::new(&mapper, o, products)
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

    Ok(ok_json(ArtefactTree::new(&evaluation, tree)))
}
