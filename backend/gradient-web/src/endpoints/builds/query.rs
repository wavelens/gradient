/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::latest_attempt_worker;
use gradient_sources::get_path_from_derivation_output;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use super::BuildAccessContext;

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildWithOutputs {
    /// Per-eval build identity (`build_job` id).
    pub id: BuildJobId,
    pub evaluation: EvaluationId,
    pub status: gradient_entity::build::BuildStatus,
    pub derivation_path: String,
    pub architecture: gradient_entity::server::Architecture,
    /// Worker identity (the `worker_id` string from `InitConnection`) that
    /// executed this build. `None` if the build never reached a worker.
    pub worker: Option<String>,
    pub output: HashMap<String, String>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

pub async fn get_build(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildJobId>,
) -> WebResult<Json<BaseResponse<BuildWithOutputs>>> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user, api_key.as_ref()).await?;
    let build_job = ctx.build_job;
    let anchor = ctx.anchor;

    let derivation = EDerivation::find_by_id(build_job.derivation)
        .one(&state.web_db)
        .await?
        .ok_or_else(|| {
            tracing::warn!(
                derivation_id = %build_job.derivation,
                %build_id,
                "Derivation not found for build"
            );
            WebError::data_inconsistency("Build")
        })?;

    let derivation_outputs = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(derivation.id))
        .all(&state.web_db)
        .await?;

    let mut outputs = HashMap::new();
    for output in derivation_outputs {
        let path = get_path_from_derivation_output(output.clone()).base();
        outputs.insert(output.name, path);
    }

    let worker = latest_attempt_worker(&state.web_db, anchor.id)
        .await
        .ok()
        .flatten();

    let build_with_outputs = BuildWithOutputs {
        id: build_job.id,
        evaluation: build_job.evaluation,
        status: anchor.status.for_api(),
        derivation_path: derivation.drv_path(),
        architecture: derivation.architecture,
        worker,
        output: outputs,
        created_at: build_job.created_at,
        updated_at: anchor.updated_at,
    };

    Ok(ok_json(build_with_outputs))
}
