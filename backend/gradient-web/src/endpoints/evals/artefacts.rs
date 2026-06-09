/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `GET /evals/{evaluation}/artefacts`
//!
//! Returns the artefact tree for an evaluation: entry points grouped by
//! derivation, derivation outputs grouped under each entry point, and
//! `build_product` rows grouped under each output. Consumed by the CLI's
//! `gradient download` artefact picker.

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::WebResult;
use crate::helpers::ok_json;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::sources::get_path_from_derivation_output;
use gradient_core::types::*;
use gradient_core::ServerState;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::EvalAccessContext;

const IS_IN_CHUNK: usize = 10_000;

#[derive(Serialize, Deserialize, Debug)]
pub struct ArtefactTree {
    pub evaluation: EvaluationId,
    pub created_at: chrono::NaiveDateTime,
    pub entry_points: Vec<EntryPointArtefacts>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EntryPointArtefacts {
    pub attr: String,
    pub derivation: String,
    pub build_id: BuildId,
    pub outputs: Vec<OutputArtefacts>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct OutputArtefacts {
    pub name: String,
    pub store_path: String,
    pub products: Vec<ProductArtefact>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ProductArtefact {
    pub id: BuildProductId,
    #[serde(rename = "type")]
    pub file_type: String,
    pub subtype: String,
    pub name: String,
    pub path: String,
    pub size: Option<i64>,
}

pub async fn get_artefacts(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(evaluation_id): Path<EvaluationId>,
) -> WebResult<Json<BaseResponse<ArtefactTree>>> {
    let ctx = EvalAccessContext::load(&state, evaluation_id, &maybe_user, api_key.as_ref()).await?;
    let evaluation = ctx.evaluation;

    let entry_points = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation.id))
        .all(&state.web_db)
        .await?;

    if entry_points.is_empty() {
        return Ok(ok_json(ArtefactTree {
            evaluation: evaluation.id,
            created_at: evaluation.created_at,
            entry_points: vec![],
        }));
    }

    let build_ids: Vec<BuildId> = entry_points
        .iter()
        .map(|ep| ep.build)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let mut builds: HashMap<BuildId, MBuild> = HashMap::new();
    for chunk in build_ids.chunks(IS_IN_CHUNK) {
        for row in EBuild::find()
            .filter(CBuild::Id.is_in(chunk.to_vec()))
            .all(&state.web_db)
            .await?
        {
            builds.insert(row.id, row);
        }
    }

    let drv_ids: Vec<DerivationId> = builds
        .values()
        .map(|b| b.derivation)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let mut derivations: HashMap<DerivationId, MDerivation> = HashMap::new();
    for chunk in drv_ids.chunks(IS_IN_CHUNK) {
        for row in EDerivation::find()
            .filter(CDerivation::Id.is_in(chunk.to_vec()))
            .all(&state.web_db)
            .await?
        {
            derivations.insert(row.id, row);
        }
    }

    let mut outputs_by_drv: HashMap<DerivationId, Vec<MDerivationOutput>> = HashMap::new();
    if !drv_ids.is_empty() {
        for chunk in drv_ids.chunks(IS_IN_CHUNK) {
            for row in EDerivationOutput::find()
                .filter(CDerivationOutput::Derivation.is_in(chunk.to_vec()))
                .all(&state.web_db)
                .await?
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
            for row in EBuildProduct::find()
                .filter(CBuildProduct::DerivationOutput.is_in(chunk.to_vec()))
                .all(&state.web_db)
                .await?
            {
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
            let build = builds.get(&ep.build)?;
            let drv = derivations.get(&build.derivation)?;
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
                            id: p.id,
                            file_type: p.file_type,
                            subtype: p.subtype,
                            name: p.name,
                            path: p.path,
                            size: p.size,
                        })
                        .collect();
                    OutputArtefacts {
                        store_path: get_path_from_derivation_output(o.clone()),
                        name: o.name,
                        products,
                    }
                })
                .collect();
            Some(EntryPointArtefacts {
                attr: ep.eval,
                derivation: drv.store_path(),
                build_id: build.id,
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

    Ok(ok_json(ArtefactTree {
        evaluation: evaluation.id,
        created_at: evaluation.created_at,
        entry_points: tree,
    }))
}
