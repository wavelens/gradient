/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use axum::extract::{Path, State};
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::HashSet;
use std::sync::Arc;

/// `GET /commits/{commit}` - returns commit metadata when the caller can
/// reach the commit through an evaluation in an organization they belong to,
/// or the org is public. Anything else maps to `404` so the endpoint never
/// confirms or denies the existence of a commit the caller can't see.
pub async fn get_commit(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(commit_id): Path<CommitId>,
) -> WebResult<Json<BaseResponse<MCommit>>> {
    let commit = ECommit::find_by_id(commit_id)
        .one(&state.web_db)
        .await?
        .or_not_found("Commit")?;

    let evaluations = EEvaluation::find()
        .filter(CEvaluation::Commit.eq(commit_id))
        .all(&state.web_db)
        .await?;

    if evaluations.is_empty() {
        return Err(WebError::not_found("Commit"));
    }

    let mut org_ids: HashSet<OrganizationId> = HashSet::new();

    let project_ids: Vec<ProjectId> = evaluations.iter().filter_map(|e| e.project).collect();
    if !project_ids.is_empty() {
        let db = &state.web_db;
        let projects = gradient_db::fetch_in_chunks(&project_ids, |chunk| async move {
            EProject::find()
                .filter(CProject::Id.is_in(chunk))
                .all(db)
                .await
        })
        .await?;
        org_ids.extend(projects.into_iter().map(|p| p.organization));
    }

    if org_ids.is_empty() {
        return Err(WebError::not_found("Commit"));
    }

    let org_id_vec: Vec<OrganizationId> = org_ids.into_iter().collect();

    let organizations = EOrganization::find()
        .filter(COrganization::Id.is_in(org_id_vec.clone()))
        .all(&state.web_db)
        .await?;

    let any_public = organizations.iter().any(|o| o.public);

    let accessible = if any_public {
        true
    } else if let Some(user) = &maybe_user {
        EOrganizationUser::find()
            .filter(COrganizationUser::User.eq(user.id))
            .filter(COrganizationUser::Organization.is_in(org_id_vec))
            .one(&state.web_db)
            .await?
            .is_some()
    } else {
        false
    };

    if !accessible {
        return Err(WebError::not_found("Commit"));
    }

    Ok(Json(BaseResponse {
        error: false,
        message: commit,
    }))
}
