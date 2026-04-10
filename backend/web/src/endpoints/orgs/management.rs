/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::authorization::MaybeUser;
use crate::endpoints::user_is_org_member;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use chrono::Utc;
use core::db::{get_any_organization_by_name, get_organization_by_name};
use core::sources::generate_ssh_key;
use core::types::consts::BASE_ROLE_ADMIN_ID;
use core::types::input::{check_index_name, validate_display_name};
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, JoinType, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeOrganizationRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub public: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PatchOrganizationRequest {
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
}

#[derive(Serialize)]
pub struct OrganizationSummary {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub public_key: Option<String>,
    pub use_nix_store: bool,
    pub public: bool,
    pub managed: bool,
    pub created_by: Uuid,
    pub created_at: chrono::NaiveDateTime,
    pub running_evaluations: i64,
}

pub async fn get_org_name_available(
    state: State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = params.get("name").cloned().unwrap_or_default();
    if check_index_name(&name).is_err() {
        return Ok(Json(BaseResponse {
            error: false,
            message: false,
        }));
    }
    let exists = EOrganization::find()
        .filter(COrganization::Name.eq(name.as_str()))
        .one(&state.db)
        .await?
        .is_some();
    Ok(Json(BaseResponse {
        error: false,
        message: !exists,
    }))
}

pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Query(params): Query<PaginationParams>,
) -> WebResult<Json<BaseResponse<Paginated<Vec<OrganizationSummary>>>>> {
    let page = params.page();
    let per_page = params.per_page();

    let paginator = EOrganization::find()
        .join_rev(
            JoinType::InnerJoin,
            EOrganizationUser::belongs_to(entity::organization::Entity)
                .from(COrganizationUser::Organization)
                .to(COrganization::Id)
                .into(),
        )
        .filter(COrganizationUser::User.eq(user.id))
        .order_by_asc(COrganization::CreatedAt)
        .paginate(&state.db, per_page);

    let total = paginator.num_items().await?;
    let orgs = paginator.fetch_page(page - 1).await?;

    // Batch-count running evaluations per organization.
    let org_ids: Vec<Uuid> = orgs.iter().map(|o| o.id).collect();
    let projects = EProject::find()
        .filter(CProject::Organization.is_in(org_ids))
        .all(&state.db)
        .await?;
    let project_ids: Vec<Uuid> = projects.iter().map(|p| p.id).collect();
    let project_to_org: HashMap<Uuid, Uuid> = projects
        .into_iter()
        .map(|p| (p.id, p.organization))
        .collect();

    let mut running_per_org: HashMap<Uuid, i64> = HashMap::new();
    if !project_ids.is_empty() {
        use entity::evaluation::EvaluationStatus;
        let running = EEvaluation::find()
            .filter(CEvaluation::Project.is_in(project_ids))
            .filter(
                Condition::any()
                    .add(CEvaluation::Status.eq(EvaluationStatus::Queued))
                    .add(CEvaluation::Status.eq(EvaluationStatus::EvaluatingFlake))
                    .add(CEvaluation::Status.eq(EvaluationStatus::EvaluatingDerivation))
                    .add(CEvaluation::Status.eq(EvaluationStatus::Building))
                    .add(CEvaluation::Status.eq(EvaluationStatus::Waiting)),
            )
            .all(&state.db)
            .await?;
        for eval in running {
            if let Some(project_id) = eval.project
                && let Some(&org_id) = project_to_org.get(&project_id)
            {
                *running_per_org.entry(org_id).or_insert(0) += 1;
            }
        }
    }

    let items: Vec<OrganizationSummary> = orgs
        .into_iter()
        .map(|o| OrganizationSummary {
            running_evaluations: *running_per_org.get(&o.id).unwrap_or(&0),
            id: o.id,
            name: o.name,
            display_name: o.display_name,
            description: o.description,
            public_key: Some(o.public_key),
            use_nix_store: o.use_nix_store,
            public: o.public,
            managed: o.managed,
            created_by: o.created_by,
            created_at: o.created_at,
        })
        .collect();

    Ok(Json(BaseResponse {
        error: false,
        message: Paginated {
            items,
            total,
            page,
            per_page,
        },
    }))
}

pub async fn put(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<MakeOrganizationRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    if check_index_name(body.name.clone().as_str()).is_err() {
        return Err(WebError::invalid_name("Organization Name"));
    }

    if let Err(e) = validate_display_name(&body.display_name) {
        return Err(WebError::BadRequest(format!("Invalid display name: {}", e)));
    }

    let existing_organization = EOrganization::find()
        .filter(COrganization::Name.eq(body.name.clone()))
        .one(&state.db)
        .await?;

    if existing_organization.is_some() {
        return Err(WebError::already_exists("Organization Name"));
    }

    let (private_key, public_key) =
        generate_ssh_key(state.cli.crypt_secret_file.clone()).map_err(|e| {
            tracing::error!("Failed to generate SSH key: {}", e);
            WebError::failed_ssh_key_generation()
        })?;

    let organization = AOrganization {
        id: Set(Uuid::new_v4()),
        name: Set(body.name.clone()),
        display_name: Set(body.display_name.trim().to_string()),
        description: Set(body.description.trim().to_string()),
        public_key: Set(public_key),
        private_key: Set(private_key),
        use_nix_store: Set(true),
        public: Set(body.public.unwrap_or(false)),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
        managed: Set(false),
        github_installation_id: Set(None),
        forge_webhook_secret: Set(None),
    };

    let organization = organization.insert(&state.db).await?;

    let organization_user = AOrganizationUser {
        id: Set(Uuid::new_v4()),
        organization: Set(organization.id),
        user: Set(user.id),
        role: Set(BASE_ROLE_ADMIN_ID),
    };

    organization_user.insert(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: organization.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_public_organizations(
    state: State<Arc<ServerState>>,
    Query(params): Query<PaginationParams>,
) -> WebResult<Json<BaseResponse<Paginated<Vec<MOrganization>>>>> {
    let page = params.page();
    let per_page = params.per_page();

    let paginator = EOrganization::find()
        .filter(COrganization::Public.eq(true))
        .order_by_asc(COrganization::CreatedAt)
        .paginate(&state.db, per_page);

    let total = paginator.num_items().await?;
    let items = paginator.fetch_page(page - 1).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: Paginated {
            items,
            total,
            page,
            per_page,
        },
    }))
}

pub async fn get_organization(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<MOrganization>>> {
    let organization: MOrganization =
        get_any_organization_by_name(state.0.clone(), organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    if !organization.public {
        match &maybe_user {
            Some(user) => {
                if !user_is_org_member(&state.0, user.id, organization.id).await? {
                    return Err(WebError::not_found("Organization"));
                }
            }
            None => return Err(WebError::not_found("Organization")),
        }
    }

    Ok(Json(BaseResponse {
        error: false,
        message: organization,
    }))
}

pub async fn patch_organization(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<PatchOrganizationRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    // Prevent modification of state-managed organizations
    if organization.managed {
        return Err(WebError::Forbidden("Cannot modify state-managed organization. This organization is managed by configuration and cannot be edited through the API.".to_string()));
    }

    let mut aorganization: AOrganization = organization.into();

    if let Some(name) = body.name {
        if check_index_name(name.as_str()).is_err() {
            return Err(WebError::invalid_name("Organization Name"));
        }

        let existing_organization = EOrganization::find()
            .filter(COrganization::Name.eq(name.clone()))
            .one(&state.db)
            .await?;

        if existing_organization.is_some() {
            return Err(WebError::already_exists("Organization Name"));
        }

        aorganization.name = Set(name);
    }

    if let Some(display_name) = body.display_name {
        let display_name = display_name.trim().to_string();
        if let Err(e) = validate_display_name(&display_name) {
            return Err(WebError::BadRequest(format!("Invalid display name: {}", e)));
        }
        aorganization.display_name = Set(display_name);
    }

    if let Some(description) = body.description {
        aorganization.description = Set(description.trim().to_string());
    }

    let organization = aorganization.update(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: organization.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn delete_organization(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    // Prevent deletion of state-managed organizations
    if organization.managed {
        return Err(WebError::Forbidden("Cannot delete state-managed organization. This organization is managed by configuration and cannot be deleted through the API.".to_string()));
    }

    let aorganization: AOrganization = organization.into();
    aorganization.delete(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "Organization deleted".to_string(),
    };

    Ok(Json(res))
}
