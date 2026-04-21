/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Link a project to named org-level integrations (inbound + outbound).

use super::{load_editable_project, load_project};
use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use core::ci::IntegrationKind;
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, Debug)]
pub struct ProjectIntegrationResponse {
    pub project: Uuid,
    pub inbound_integration: Option<Uuid>,
    pub outbound_integration: Option<Uuid>,
}

impl From<MProjectIntegration> for ProjectIntegrationResponse {
    fn from(m: MProjectIntegration) -> Self {
        ProjectIntegrationResponse {
            project: m.project,
            inbound_integration: m.inbound_integration,
            outbound_integration: m.outbound_integration,
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct PutProjectIntegrationRequest {
    pub inbound_integration: Option<Uuid>,
    pub outbound_integration: Option<Uuid>,
}

async fn validate_integration(
    state: &Arc<ServerState>,
    org_id: Uuid,
    integration_id: Uuid,
    expected_kind: IntegrationKind,
) -> WebResult<()> {
    let row = EIntegration::find()
        .filter(CIntegration::Id.eq(integration_id))
        .filter(CIntegration::Organization.eq(org_id))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Integration"))?;

    if row.kind != expected_kind.as_i16() {
        return Err(WebError::BadRequest(format!(
            "Integration {} is not {}.",
            integration_id,
            match expected_kind {
                IntegrationKind::Inbound => "inbound",
                IntegrationKind::Outbound => "outbound",
            }
        )));
    }
    Ok(())
}

/// `GET /projects/{organization}/{project}/integration` — current link row
/// (returns nulls when no link exists).
pub async fn get_project_integration(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<ProjectIntegrationResponse>>> {
    let user =
        maybe_user.ok_or_else(|| WebError::Unauthorized("Authentication required.".to_string()))?;
    let (_org, proj) = load_project(&state, user.id, organization, project).await?;

    let link = EProjectIntegration::find_by_id(proj.id)
        .one(&state.db)
        .await?;

    let message = match link {
        Some(l) => ProjectIntegrationResponse::from(l),
        None => ProjectIntegrationResponse {
            project: proj.id,
            inbound_integration: None,
            outbound_integration: None,
        },
    };

    Ok(Json(BaseResponse {
        error: false,
        message,
    }))
}

/// `PUT /projects/{organization}/{project}/integration` — upsert the link row.
pub async fn put_project_integration(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project)): Path<(String, String)>,
    Json(body): Json<PutProjectIntegrationRequest>,
) -> WebResult<Json<BaseResponse<ProjectIntegrationResponse>>> {
    let (org, proj) = load_editable_project(&state, user.id, organization, project).await?;

    if let Some(id) = body.inbound_integration {
        validate_integration(&state, org.id, id, IntegrationKind::Inbound).await?;
    }
    if let Some(id) = body.outbound_integration {
        validate_integration(&state, org.id, id, IntegrationKind::Outbound).await?;
    }

    let existing = EProjectIntegration::find_by_id(proj.id)
        .one(&state.db)
        .await?;

    let updated = match existing {
        Some(row) => {
            let mut active = row.into_active_model();
            active.inbound_integration = Set(body.inbound_integration);
            active.outbound_integration = Set(body.outbound_integration);
            active.update(&state.db).await?
        }
        None => {
            AProjectIntegration {
                project: Set(proj.id),
                inbound_integration: Set(body.inbound_integration),
                outbound_integration: Set(body.outbound_integration),
            }
            .insert(&state.db)
            .await?
        }
    };

    Ok(Json(BaseResponse {
        error: false,
        message: ProjectIntegrationResponse::from(updated),
    }))
}

/// `DELETE /projects/{organization}/{project}/integration` — remove link row.
pub async fn delete_project_integration(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, project)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let (_org, proj) = load_editable_project(&state, user.id, organization, project).await?;

    if let Some(row) = EProjectIntegration::find_by_id(proj.id)
        .one(&state.db)
        .await?
    {
        row.into_active_model().delete(&state.db).await?;
    }

    Ok(Json(BaseResponse {
        error: false,
        message: true,
    }))
}
