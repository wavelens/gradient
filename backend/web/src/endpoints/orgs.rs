/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::endpoints::user_is_org_member;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use chrono::Utc;
use core::consts::BASE_ROLE_ADMIN_ID;
use core::database::{get_any_organization_by_name, get_cache_by_name, get_organization_by_name};
use core::input::{check_index_name, validate_display_name};
use core::sources::{format_public_key, generate_ssh_key};
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, JoinType, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, RelationTrait,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StringListItem {
    pub id: String,
    pub name: String,
}
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

#[derive(Serialize, Deserialize, Debug)]
pub struct AddUserRequest {
    pub user: String,
    pub role: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RemoveUserRequest {
    pub user: String,
}

pub async fn get_org_name_available(
    state: State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> WebResult<Json<BaseResponse<bool>>> {
    let name = params.get("name").cloned().unwrap_or_default();
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
) -> WebResult<Json<BaseResponse<Paginated<Vec<MOrganization>>>>> {
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
    let items = paginator.fetch_page(page - 1).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: Paginated { items, total, page, per_page },
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
        display_name: Set(body.display_name.clone()),
        description: Set(body.description.clone()),
        public_key: Set(public_key),
        private_key: Set(private_key),
        use_nix_store: Set(true),
        public: Set(body.public.unwrap_or(false)),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
        managed: Set(false),
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
    Extension(_user): Extension<MUser>,
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
        message: Paginated { items, total, page, per_page },
    }))
}

pub async fn get_organization(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<MOrganization>>> {
    let organization: MOrganization =
        get_any_organization_by_name(state.0.clone(), organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    if !organization.public {
        let is_member = user_is_org_member(&state.0, user.id, organization.id).await?;
        if !is_member {
            return Err(WebError::not_found("Organization"));
        }
    }

    let res = BaseResponse {
        error: false,
        message: organization,
    };

    Ok(Json(res))
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
        if let Err(e) = validate_display_name(&display_name) {
            return Err(WebError::BadRequest(format!("Invalid display name: {}", e)));
        }
        aorganization.display_name = Set(display_name);
    }

    if let Some(description) = body.description {
        aorganization.description = Set(description);
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

pub async fn get_organization_users(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<Vec<StringListItem>>>> {
    let organization: MOrganization =
        get_any_organization_by_name(state.0.clone(), organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    if !organization.public {
        let is_member = user_is_org_member(&state.0, user.id, organization.id).await?;
        if !is_member {
            return Err(WebError::not_found("Organization"));
        }
    }

    let organization_users = EOrganizationUser::find()
        .join(JoinType::InnerJoin, ROrganizationUser::User.def())
        .select_also(entity::user::Entity)
        .filter(COrganizationUser::Organization.eq(organization.id))
        .all(&state.db)
        .await?;

    let role_ids: Vec<Uuid> = organization_users.iter().map(|(ou, _)| ou.role).collect();
    let role_map: std::collections::HashMap<Uuid, String> = ERole::find()
        .filter(CRole::Id.is_in(role_ids))
        .all(&state.db)
        .await?
        .into_iter()
        .map(|r| (r.id, r.name))
        .collect();

    let organization_users: Vec<StringListItem> = organization_users
        .iter()
        .map(|(ou, user)| StringListItem {
            id: user
                .as_ref()
                .map(|u| u.username.clone())
                .unwrap_or_else(|| ou.user.to_string()),
            name: role_map
                .get(&ou.role)
                .cloned()
                .unwrap_or_else(|| ou.role.to_string()),
        })
        .collect();

    let res = BaseResponse {
        error: false,
        message: organization_users,
    };

    Ok(Json(res))
}

pub async fn post_organization_users(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<AddUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let target_user = EUser::find()
        .filter(CUser::Username.eq(body.user.clone()))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("User"))?;

    let organization_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization.id))
                .add(COrganizationUser::User.eq(target_user.id)),
        )
        .one(&state.db)
        .await?;

    if organization_user.is_some() {
        return Err(WebError::already_exists("User already in Organization"));
    }

    let role = ERole::find()
        .filter(
            Condition::all().add(CRole::Name.eq(body.role.clone())).add(
                Condition::any()
                    .add(CRole::Organization.eq(organization.id))
                    .add(CRole::Organization.is_null()),
            ),
        )
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Role"))?;

    let organization_user = AOrganizationUser {
        id: Set(Uuid::new_v4()),
        organization: Set(organization.id),
        user: Set(target_user.id),
        role: Set(role.id),
    };

    organization_user.insert(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "User invited".to_string(),
    };

    Ok(Json(res))
}

pub async fn patch_organization_users(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<AddUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let target_user = EUser::find()
        .filter(CUser::Username.eq(body.user.clone()))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("User"))?;

    let organization_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization.id))
                .add(COrganizationUser::User.eq(target_user.id)),
        )
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::BadRequest("User not in Organization".to_string()))?;

    let role = ERole::find()
        .filter(CRole::Name.eq(body.role.clone()))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("Role"))?;

    let mut aorganization_user: AOrganizationUser = organization_user.into();
    aorganization_user.role = Set(role.id);
    aorganization_user.update(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "User role updated".to_string(),
    };

    Ok(Json(res))
}

pub async fn delete_organization_users(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
    Json(body): Json<RemoveUserRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let target_user = EUser::find()
        .filter(CUser::Username.eq(body.user.clone()))
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::not_found("User"))?;

    let organization_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization.id))
                .add(COrganizationUser::User.eq(target_user.id)),
        )
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::BadRequest("User not in Organization".to_string()))?;

    let aorganization_user: AOrganizationUser = organization_user.into();
    aorganization_user.delete(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "User kicked".to_string(),
    };

    Ok(Json(res))
}

pub async fn get_organization_ssh(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let res = BaseResponse {
        error: false,
        message: format_public_key(organization),
    };

    Ok(Json(res))
}

pub async fn post_organization_ssh(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    if organization.managed {
        return Err(WebError::Forbidden(
            "Cannot regenerate SSH key for state-managed organization. This organization is managed by configuration and cannot have its SSH key modified through the API.".to_string(),
        ));
    }

    let (private_key, public_key) =
        generate_ssh_key(state.cli.crypt_secret_file.clone()).map_err(|e| {
            tracing::error!("Failed to generate SSH key: {}", e);
            WebError::failed_ssh_key_generation()
        })?;

    let mut aorganization: AOrganization = organization.into();

    aorganization.private_key = Set(private_key.clone());
    aorganization.public_key = Set(public_key.clone());
    let organization = aorganization.update(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: format_public_key(organization),
    };

    Ok(Json(res))
}

pub async fn post_organization_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    if organization.managed {
        return Err(WebError::Forbidden(
            "Cannot modify state-managed organization.".to_string(),
        ));
    }

    let mut aorganization: AOrganization = organization.into();
    aorganization.public = Set(true);
    aorganization.update(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Organization is now public".to_string(),
    }))
}

pub async fn delete_organization_public(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    if organization.managed {
        return Err(WebError::Forbidden(
            "Cannot modify state-managed organization.".to_string(),
        ));
    }

    let mut aorganization: AOrganization = organization.into();
    aorganization.public = Set(false);
    aorganization.update(&state.db).await?;

    Ok(Json(BaseResponse {
        error: false,
        message: "Organization is now private".to_string(),
    }))
}

pub async fn get_organization_subscribe(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> WebResult<Json<BaseResponse<ListResponse>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let organization_caches = EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(organization.id))
        .all(&state.db)
        .await?;

    let mut subscribed: ListResponse = Vec::new();
    for oc in organization_caches {
        if let Ok(Some(cache)) = ECache::find_by_id(oc.cache).one(&state.db).await {
            subscribed.push(ListItem {
                id: oc.cache,
                name: cache.name,
            });
        }
    }

    let res = BaseResponse {
        error: false,
        message: subscribed,
    };

    Ok(Json(res))
}

pub async fn post_organization_subscribe_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, cache)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let cache: MCache = get_cache_by_name(state.0.clone(), user.id, cache.clone())
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    let organization_cache = EOrganizationCache::find()
        .filter(
            Condition::all()
                .add(COrganizationCache::Organization.eq(organization.id))
                .add(COrganizationCache::Cache.eq(cache.id)),
        )
        .one(&state.db)
        .await?;

    if organization_cache.is_some() {
        return Err(WebError::already_exists(
            "Organization already subscribed to Cache",
        ));
    }

    let aorganization_cache = AOrganizationCache {
        id: Set(Uuid::new_v4()),
        organization: Set(organization.id),
        cache: Set(cache.id),
    };

    aorganization_cache.insert(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "Cache subscribed".to_string(),
    };

    Ok(Json(res))
}

pub async fn delete_organization_subscribe_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, cache)): Path<(String, String)>,
) -> WebResult<Json<BaseResponse<String>>> {
    let organization: MOrganization =
        get_organization_by_name(state.0.clone(), user.id, organization.clone())
            .await?
            .ok_or_else(|| WebError::not_found("Organization"))?;

    let cache: MCache = get_cache_by_name(state.0.clone(), user.id, cache.clone())
        .await?
        .ok_or_else(|| WebError::not_found("Cache"))?;

    let organization_cache = EOrganizationCache::find()
        .filter(
            Condition::all()
                .add(COrganizationCache::Organization.eq(organization.id))
                .add(COrganizationCache::Cache.eq(cache.id)),
        )
        .one(&state.db)
        .await?
        .ok_or_else(|| WebError::BadRequest("Organization not subscribed to Cache".to_string()))?;

    let aorganization_cache: AOrganizationCache = organization_cache.into();
    aorganization_cache.delete(&state.db).await?;

    let res = BaseResponse {
        error: false,
        message: "Cache unsubscribed".to_string(),
    };

    Ok(Json(res))
}
