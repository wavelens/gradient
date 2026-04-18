/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use core::types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::BuildAccessContext;

#[derive(Serialize, Deserialize, Debug)]
pub struct BuildWithOutputs {
    pub id: Uuid,
    pub evaluation: Uuid,
    pub status: entity::build::BuildStatus,
    pub derivation_path: String,
    pub architecture: entity::server::Architecture,
    pub server: Option<Uuid>,
    pub output: HashMap<String, String>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

pub async fn get_build(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(build_id): Path<Uuid>,
) -> WebResult<Json<BaseResponse<BuildWithOutputs>>> {
    let ctx = BuildAccessContext::load(&state, build_id, &maybe_user).await?;
    let build = ctx.build;

    let derivation = EDerivation::find_by_id(build.derivation)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!(
                "Derivation {} not found for build {}",
                build.derivation,
                build_id
            );
            WebError::InternalServerError("Build data inconsistency".to_string())
        })?;

    let derivation_outputs = EDerivationOutput::find()
        .filter(CDerivationOutput::Derivation.eq(derivation.id))
        .all(&state.db)
        .await?;

    let mut outputs = HashMap::new();
    for output in derivation_outputs {
        outputs.insert(output.name, output.output);
    }

    let build_with_outputs = BuildWithOutputs {
        id: build.id,
        evaluation: build.evaluation,
        status: build.status,
        derivation_path: derivation.derivation_path,
        architecture: derivation.architecture,
        server: build.server,
        output: outputs,
        created_at: build.created_at,
        updated_at: build.updated_at,
    };

    Ok(Json(BaseResponse {
        error: false,
        message: build_with_outputs,
    }))
}
