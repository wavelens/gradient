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
use gradient_core::sources::get_path_from_derivation_output;
use gradient_core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use super::BuildAccessContext;

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildWithOutputs {
    pub id: BuildId,
    pub evaluation: EvaluationId,
    pub status: gradient_entity::build::BuildStatus,
    pub derivation_path: String,
    pub architecture: gradient_entity::server::Architecture,
    /// Worker identity (the `worker_id` string from `InitConnection`) that
    /// executed this build. `None` if the build never reached a worker.
    pub worker: Option<String>,
    /// When set, this build is a follower of another build (same derivation,
    /// different evaluation) whose terminal status will be copied here. The
    /// scheduler does not dispatch builds with `via` set.
    pub via: Option<BuildId>,
    pub output: HashMap<String, String>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

pub async fn get_build(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(build_id): Path<BuildId>,
) -> WebResult<Json<BaseResponse<BuildWithOutputs>>> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user, api_key.as_ref()).await?;
    let build = ctx.build;

    let derivation = EDerivation::find_by_id(build.derivation)
        .one(&state.web_db)
        .await?
        .ok_or_else(|| {
            tracing::warn!(
                derivation_id = %build.derivation,
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
        let path = get_path_from_derivation_output(output.clone());
        outputs.insert(output.name, path);
    }

    let build_with_outputs = BuildWithOutputs {
        id: build.id,
        evaluation: build.evaluation,
        status: build.status.for_api(),
        derivation_path: derivation.store_path(),
        architecture: derivation.architecture,
        worker: build.worker,
        via: build.via,
        output: outputs,
        created_at: build.created_at,
        updated_at: build.updated_at,
    };

    Ok(ok_json(build_with_outputs))
}
