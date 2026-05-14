/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, OrgAccess, load_org};
use crate::audit::{RequestInfo, events, record as audit_record};
use crate::authorization::{MaybeApiKey, MaybeUser};
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::permissions::Permission;
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};

use gradient_core::sources::generate_ssh_key;
use gradient_core::types::consts::BASE_ROLE_ADMIN_ID;
use gradient_core::types::input::{check_index_name, validate_display_name};
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, JoinType, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

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
    pub id: OrganizationId,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub public_key: Option<String>,
    pub public: bool,
    pub managed: bool,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub running_evaluations: i64,
    pub role: Option<String>,
}

#[derive(Serialize)]
pub struct OrgResponse {
    pub id: OrganizationId,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub public_key: Option<String>,
    pub public: bool,
    pub managed: bool,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    /// GitHub App installation id for this org. `Some` is the single signal
    /// that the org uses the GitHub App; outbound CI status reporting and
    /// install-webhook routing both gate on this being populated.
    pub github_installation_id: Option<i64>,
    /// Whether the server has a GitHub App configured at all. The frontend
    /// hides GitHub-specific UI entirely when this is `false`.
    pub github_app_available: bool,
    pub role: Option<String>,
}

pub async fn get_org_name_available(
    state: State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = params.get("name").cloned().unwrap_or_default();
    if check_index_name(&name).is_err() {
        return Ok(ok_json(false));
    }
    let exists = EOrganization::find()
        .filter(COrganization::Name.eq(name.as_str()))
        .one(&state.web_db)
        .await?
        .is_some();
    Ok(ok_json(!exists))
}

/// Count in-progress evaluations per organization for `org_ids`.
///
/// Returns a map of org_id → count of evaluations in any active status
/// (Queued, Fetching, EvaluatingFlake, EvaluatingDerivation, Building, Waiting).
async fn count_running_evaluations(
    state: &Arc<ServerState>,
    org_ids: &[OrganizationId],
) -> WebResult<HashMap<OrganizationId, i64>> {
    use entity::evaluation::EvaluationStatus;

    if org_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let projects = EProject::find()
        .filter(CProject::Organization.is_in(org_ids.to_vec()))
        .all(&state.web_db)
        .await?;

    let project_ids: Vec<ProjectId> = projects.iter().map(|p| p.id).collect();
    let project_to_org: HashMap<ProjectId, OrganizationId> = projects
        .into_iter()
        .map(|p| (p.id, p.organization))
        .collect();

    let mut running_per_org: HashMap<OrganizationId, i64> = HashMap::new();
    if !project_ids.is_empty() {
        let running = EEvaluation::find()
            .filter(CEvaluation::Project.is_in(project_ids))
            .filter(CEvaluation::Status.is_in(EvaluationStatus::ACTIVE))
            .all(&state.web_db)
            .await?;
        for eval in running {
            if let Some(project_id) = eval.project
                && let Some(&org_id) = project_to_org.get(&project_id)
            {
                *running_per_org.entry(org_id).or_insert(0) += 1;
            }
        }
    }

    Ok(running_per_org)
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
        .paginate(&state.web_db, per_page);

    let total = paginator.num_items().await?;
    let orgs = paginator.fetch_page(page - 1).await?;

    let org_ids: Vec<OrganizationId> = orgs.iter().map(|o| o.id).collect();
    let running_per_org = count_running_evaluations(&state, &org_ids).await?;

    // Fetch the current user's membership row for each org to get the role.
    let org_users = EOrganizationUser::find()
        .filter(COrganizationUser::User.eq(user.id))
        .filter(COrganizationUser::Organization.is_in(org_ids.clone()))
        .all(&state.web_db)
        .await?;

    let role_ids: Vec<RoleId> = org_users.iter().map(|ou| ou.role).collect();
    let roles = ERole::find()
        .filter(CRole::Id.is_in(role_ids))
        .all(&state.web_db)
        .await?;
    let role_name_map: HashMap<RoleId, String> =
        roles.into_iter().map(|r| (r.id, r.name)).collect();
    let org_role_map: HashMap<OrganizationId, String> = org_users
        .into_iter()
        .filter_map(|ou| {
            role_name_map
                .get(&ou.role)
                .map(|n| (ou.organization, n.clone()))
        })
        .collect();

    let items: Vec<OrganizationSummary> = orgs
        .into_iter()
        .map(|o| OrganizationSummary {
            running_evaluations: *running_per_org.get(&o.id).unwrap_or(&0),
            role: org_role_map.get(&o.id).cloned(),
            id: o.id,
            name: o.name,
            display_name: o.display_name,
            description: o.description,
            public_key: Some(o.public_key),
            public: o.public,
            managed: o.managed,
            created_by: o.created_by,
            created_at: o.created_at,
        })
        .collect();

    Ok(ok_json(Paginated {
        items,
        total,
        page,
        per_page,
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
        return Err(WebError::bad_request(format!(
            "Invalid display name: {}",
            e
        )));
    }

    let existing_organization = EOrganization::find()
        .filter(COrganization::Name.eq(body.name.clone()))
        .one(&state.web_db)
        .await?;

    if existing_organization.is_some() {
        return Err(WebError::already_exists("Organization Name"));
    }

    let (private_key, public_key) = generate_ssh_key(&state.config.secrets.crypt_secret_file)
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to generate SSH key");
            WebError::failed_ssh_key_generation()
        })?;

    let tx = state.web_db.inner().begin().await?;

    let organization = AOrganization {
        id: Set(OrganizationId::now_v7()),
        name: Set(body.name.clone()),
        display_name: Set(body.display_name.trim().to_string()),
        description: Set(body.description.trim().to_string()),
        public_key: Set(public_key),
        private_key: Set(private_key),
        public: Set(body.public.unwrap_or(false)),
        created_by: Set(user.id),
        created_at: Set(gradient_core::types::now()),
        managed: Set(false),
        github_installation_id: Set(None),
    }
    .insert(&tx)
    .await
    .map_err(|e| WebError::from_db_err(e, "Organization Name"))?;

    AOrganizationUser {
        id: Set(OrganizationUserId::now_v7()),
        organization: Set(organization.id),
        user: Set(user.id),
        role: Set(BASE_ROLE_ADMIN_ID),
    }
    .insert(&tx)
    .await?;

    tx.commit().await?;

    Ok(Json(BaseResponse {
        error: false,
        message: organization.id.to_string(),
    }))
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
        .paginate(&state.web_db, per_page);

    let total = paginator.num_items().await?;
    let items = paginator.fetch_page(page - 1).await?;

    Ok(ok_json(Paginated {
        items,
        total,
        page,
        per_page,
    }))
}

pub async fn get_organization(
    state: State<Arc<ServerState>>,
    Extension(MaybeUser(maybe_user)): Extension<MaybeUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<OrgResponse>>> {
    let org = load_org(
        &state.0,
        Caller::from_option(&maybe_user),
        api_key.as_ref(),
        organization,
        OrgAccess::Readable {
            label: "Organization",
        },
    )
    .await?;

    let role = if let Some(ref user) = maybe_user {
        let org_user = EOrganizationUser::find()
            .filter(COrganizationUser::User.eq(user.id))
            .filter(COrganizationUser::Organization.eq(org.id))
            .one(&state.web_db)
            .await?;
        if let Some(ou) = org_user {
            ERole::find_by_id(ou.role)
                .one(&state.web_db)
                .await?
                .map(|r| r.name)
        } else {
            None
        }
    } else {
        None
    };

    Ok(ok_json(OrgResponse {
        id: org.id,
        name: org.name,
        display_name: org.display_name,
        description: org.description,
        public_key: Some(org.public_key),
        public: org.public,
        managed: org.managed,
        created_by: org.created_by,
        created_at: org.created_at,
        github_installation_id: org.github_installation_id,
        github_app_available: state.config.github_app.clone().is_some(),
        role,
    }))
}

pub async fn patch_organization(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(organization): Path<String>,
    Json(body): Json<PatchOrganizationRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Require {
            permission: Permission::ManageOrgSettings,
            reject_managed: true,
        },
    )
    .await?;
    let mut aorganization: AOrganization = organization.into();

    if let Some(name) = body.name {
        if check_index_name(name.as_str()).is_err() {
            return Err(WebError::invalid_name("Organization Name"));
        }

        let existing_organization = EOrganization::find()
            .filter(COrganization::Name.eq(name.clone()))
            .one(&state.web_db)
            .await?;

        if existing_organization.is_some() {
            return Err(WebError::already_exists("Organization Name"));
        }

        aorganization.name = Set(name);
    }

    if let Some(display_name) = body.display_name {
        let display_name = display_name.trim().to_string();
        if let Err(e) = validate_display_name(&display_name) {
            return Err(WebError::bad_request(format!(
                "Invalid display name: {}",
                e
            )));
        }
        aorganization.display_name = Set(display_name);
    }

    crate::patch_field_with!(aorganization, body, description, |s: String| s
        .trim()
        .to_string());

    let organization = aorganization
        .update(&state.web_db)
        .await
        .map_err(|e| WebError::from_db_err(e, "Organization Name"))?;

    let res = BaseResponse {
        error: false,
        message: organization.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn delete_organization(
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization = load_org(
        &state,
        Caller::User(&user),
        api_key.as_ref(),
        organization,
        OrgAccess::Require {
            permission: Permission::DeleteOrg,
            reject_managed: true,
        },
    )
    .await?;
    let org_id = organization.id;
    let org_name = organization.name.clone();
    let aorganization: AOrganization = organization.into();
    aorganization.delete(&state.web_db).await?;

    audit_record(
        &state.web_db,
        Some(user.id),
        events::ORG_DELETE,
        &info,
        Some(serde_json::json!({
            "organization_id": org_id.to_string(),
            "organization_name": org_name,
        })),
    )
    .await;

    let res = BaseResponse {
        error: false,
        message: "Organization deleted".to_string(),
    };

    Ok(Json(res))
}
