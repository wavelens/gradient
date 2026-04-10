/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::endpoints::user_is_org_member;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use core::types::*;
use sea_orm::EntityTrait;
use sea_orm::{ColumnTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

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
    let build = EBuild::find_by_id(build_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Build"))?;

    let evaluation = EEvaluation::find_by_id(build.evaluation)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!(
                "Evaluation {} not found for build {}",
                build.evaluation,
                build_id
            );
            WebError::InternalServerError("Build data inconsistency".to_string())
        })?;

    let organization_id = if let Some(project_id) = evaluation.project {
        let project = EProject::find_by_id(project_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!(
                    "Project {} not found for evaluation {}",
                    project_id,
                    evaluation.id
                );
                WebError::InternalServerError("Evaluation data inconsistency".to_string())
            })?;
        project.organization
    } else {
        EDirectBuild::find()
            .filter(CDirectBuild::Evaluation.eq(evaluation.id))
            .one(&state.db)
            .await?
            .ok_or_else(|| {
                tracing::error!("DirectBuild not found for evaluation {}", evaluation.id);
                WebError::InternalServerError("Direct build data inconsistency".to_string())
            })?
            .organization
    };
    let organization = EOrganization::find_by_id(organization_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| {
            tracing::error!("Organization {} not found", organization_id);
            WebError::InternalServerError("Organization data inconsistency".to_string())
        })?;

    let can_access = if organization.public {
        true
    } else {
        match &maybe_user {
            Some(user) => user_is_org_member(&state, user.id, organization.id).await?,
            None => false,
        }
    };
    if !can_access {
        return Err(WebError::not_found("Build"));
    }

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

    let res = BaseResponse {
        error: false,
        message: build_with_outputs,
    };

    Ok(Json(res))
}
