/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use chrono::Utc;
use core::consts::BASE_ROLE_ADMIN_ID;
use core::database::{get_cache_by_name, get_organization_by_name};
use core::input::check_index_name;
use core::sources::{format_public_key, generate_ssh_key};
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, JoinType, QueryFilter, QuerySelect,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug)]
pub struct MakeOrganizationRequest {
    pub name: String,
    pub display_name: String,
    pub description: String,
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

pub async fn get(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    // TODO: Implement pagination
    let organizations = EOrganization::find()
        .join_rev(
            JoinType::InnerJoin,
            EOrganizationUser::belongs_to(entity::organization::Entity)
                .from(COrganizationUser::Organization)
                .to(COrganization::Id)
                .into(),
        )
        .filter(COrganizationUser::User.eq(user.id))
        .all(&state.db)
        .await
        .unwrap();

    let organizations: ListResponse = organizations
        .iter()
        .map(|o| ListItem {
            id: o.id,
            name: o.name.clone(),
        })
        .collect();

    let res = BaseResponse {
        error: false,
        message: organizations,
    };

    Ok(Json(res))
}

pub async fn put(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Json(body): Json<MakeOrganizationRequest>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    if check_index_name(body.name.clone().as_str()).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(BaseResponse {
                error: true,
                message: "Invalid Organization Name".to_string(),
            }),
        ));
    }

    let organization = EOrganization::find()
        .filter(COrganization::Name.eq(body.name.clone()))
        .one(&state.db)
        .await
        .unwrap();

    if organization.is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(BaseResponse {
                error: true,
                message: "Organization Name already exists".to_string(),
            }),
        ));
    }

    let (private_key, public_key) =
        generate_ssh_key(state.cli.crypt_secret_file.clone()).map_err(|e| {
            println!("{}", e);

            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BaseResponse {
                    error: true,
                    message: "Failed to generate SSH key".to_string(),
                }),
            )
        })?;

    let organization = AOrganization {
        id: Set(Uuid::new_v4()),
        name: Set(body.name.clone()),
        display_name: Set(body.display_name.clone()),
        description: Set(body.description.clone()),
        public_key: Set(public_key),
        private_key: Set(private_key),
        use_nix_store: Set(true),
        created_by: Set(user.id),
        created_at: Set(Utc::now().naive_utc()),
    };

    let organization = organization.insert(&state.db).await.unwrap();

    let organization_user = AOrganizationUser {
        id: Set(Uuid::new_v4()),
        organization: Set(organization.id),
        user: Set(user.id),
        role: Set(BASE_ROLE_ADMIN_ID),
    };

    organization_user.insert(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: organization.id.to_string(),
    };

    Ok(Json(res))
}

pub async fn get_organization(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> Result<Json<BaseResponse<MOrganization>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization: MOrganization =
        match get_organization_by_name(state.0.clone(), user.id, organization.clone()).await {
            Some(o) => o,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Organization not found".to_string(),
                    }),
                ))
            }
        };

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
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization: MOrganization =
        match get_organization_by_name(state.0.clone(), user.id, organization.clone()).await {
            Some(o) => o,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Organization not found".to_string(),
                    }),
                ))
            }
        };

    let mut aorganization: AOrganization = organization.into();

    if let Some(name) = body.name {
        if check_index_name(name.as_str()).is_err() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(BaseResponse {
                    error: true,
                    message: "Invalid Organization Name".to_string(),
                }),
            ));
        }

        let organization = EOrganization::find()
            .filter(COrganization::Name.eq(name.clone()))
            .one(&state.db)
            .await
            .unwrap();

        if organization.is_some() {
            return Err((
                StatusCode::CONFLICT,
                Json(BaseResponse {
                    error: true,
                    message: "Organization Name already exists".to_string(),
                }),
            ));
        }

        aorganization.name = Set(name);
    }

    if let Some(display_name) = body.display_name {
        aorganization.display_name = Set(display_name);
    }

    if let Some(description) = body.description {
        aorganization.description = Set(description);
    }

    let organization = aorganization.update(&state.db).await.unwrap();

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
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization: MOrganization =
        match get_organization_by_name(state.0.clone(), user.id, organization.clone()).await {
            Some(o) => o,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Organization not found".to_string(),
                    }),
                ))
            }
        };

    let aorganization: AOrganization = organization.into();
    aorganization.delete(&state.db).await.unwrap();

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
) -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization: MOrganization =
        match get_organization_by_name(state.0.clone(), user.id, organization.clone()).await {
            Some(o) => o,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Organization not found".to_string(),
                    }),
                ))
            }
        };

    let organization_users = EOrganizationUser::find()
        .filter(COrganizationUser::Organization.eq(organization.id))
        .all(&state.db)
        .await
        .unwrap();

    let organization_users: ListResponse = organization_users
        .iter()
        .map(|ou| ListItem {
            id: ou.user,
            name: ou.role.to_string(),
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
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization: MOrganization =
        match get_organization_by_name(state.0.clone(), user.id, organization.clone()).await {
            Some(o) => o,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Organization not found".to_string(),
                    }),
                ))
            }
        };

    let user = EUser::find()
        .filter(CUser::Name.eq(body.user.clone()))
        .one(&state.db)
        .await
        .unwrap();

    if user.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "User not found".to_string(),
            }),
        ));
    }

    let user = user.unwrap();

    let organization_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization.id))
                .add(COrganizationUser::User.eq(user.id)),
        )
        .one(&state.db)
        .await
        .unwrap();

    if organization_user.is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(BaseResponse {
                error: true,
                message: "User already in Organization".to_string(),
            }),
        ));
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
        .await
        .unwrap();

    if role.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Role not found".to_string(),
            }),
        ));
    }

    let role = role.unwrap();

    let organization_user = AOrganizationUser {
        id: Set(Uuid::new_v4()),
        organization: Set(organization.id),
        user: Set(user.id),
        role: Set(role.id),
    };

    organization_user.insert(&state.db).await.unwrap();

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
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization: MOrganization =
        match get_organization_by_name(state.0.clone(), user.id, organization.clone()).await {
            Some(o) => o,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Organization not found".to_string(),
                    }),
                ))
            }
        };

    let user = EUser::find()
        .filter(CUser::Name.eq(body.user.clone()))
        .one(&state.db)
        .await
        .unwrap();

    if user.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "User not found".to_string(),
            }),
        ));
    }

    let user = user.unwrap();

    let organization_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization.id))
                .add(COrganizationUser::User.eq(user.id)),
        )
        .one(&state.db)
        .await
        .unwrap();

    if organization_user.is_none() {
        return Err((
            StatusCode::CONFLICT,
            Json(BaseResponse {
                error: true,
                message: "User not in Organization".to_string(),
            }),
        ));
    }

    let organization_user = organization_user.unwrap();

    let role = ERole::find()
        .filter(CRole::Name.eq(body.role.clone()))
        .one(&state.db)
        .await
        .unwrap();

    if role.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "Role not found".to_string(),
            }),
        ));
    }

    let role = role.unwrap();

    let mut aorganization_user: AOrganizationUser = organization_user.into();
    aorganization_user.role = Set(role.id);
    aorganization_user.update(&state.db).await.unwrap();

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
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization: MOrganization =
        match get_organization_by_name(state.0.clone(), user.id, organization.clone()).await {
            Some(o) => o,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Organization not found".to_string(),
                    }),
                ))
            }
        };

    let user = EUser::find()
        .filter(CUser::Name.eq(body.user.clone()))
        .one(&state.db)
        .await
        .unwrap();

    if user.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(BaseResponse {
                error: true,
                message: "User not found".to_string(),
            }),
        ));
    }

    let user = user.unwrap();

    let organization_user = EOrganizationUser::find()
        .filter(
            Condition::all()
                .add(COrganizationUser::Organization.eq(organization.id))
                .add(COrganizationUser::User.eq(user.id)),
        )
        .one(&state.db)
        .await
        .unwrap();

    if organization_user.is_none() {
        return Err((
            StatusCode::CONFLICT,
            Json(BaseResponse {
                error: true,
                message: "User not in Organization".to_string(),
            }),
        ));
    }

    let organization_user = organization_user.unwrap();

    let aorganization_user: AOrganizationUser = organization_user.into();
    aorganization_user.delete(&state.db).await.unwrap();

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
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization: MOrganization =
        match get_organization_by_name(state.0.clone(), user.id, organization.clone()).await {
            Some(o) => o,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Organization not found".to_string(),
                    }),
                ))
            }
        };

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
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization: MOrganization =
        match get_organization_by_name(state.0.clone(), user.id, organization.clone()).await {
            Some(o) => o,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Organization not found".to_string(),
                    }),
                ))
            }
        };

    let (private_key, public_key) = generate_ssh_key(state.cli.crypt_secret_file.clone()).unwrap();

    let mut aorganization: AOrganization = organization.into();

    aorganization.private_key = Set(private_key.clone());
    aorganization.public_key = Set(public_key.clone());
    let organization = aorganization.update(&state.db).await.unwrap();

    let res = BaseResponse {
        error: false,
        message: format_public_key(organization),
    };

    Ok(Json(res))
}

pub async fn get_organization_subscribe(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path(organization): Path<String>,
) -> Result<Json<BaseResponse<ListResponse>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization: MOrganization =
        match get_organization_by_name(state.0.clone(), user.id, organization.clone()).await {
            Some(o) => o,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Organization not found".to_string(),
                    }),
                ))
            }
        };

    let organization_caches = EOrganizationCache::find()
        .filter(COrganizationCache::Organization.eq(organization.id))
        .all(&state.db)
        .await
        .unwrap();

    let organization_users: ListResponse = organization_caches
        .iter()
        .map(|ou| ListItem {
            id: ou.cache,
            name: ou.cache.to_string(),
        })
        .collect();

    let res = BaseResponse {
        error: false,
        message: organization_users,
    };

    Ok(Json(res))
}

pub async fn post_organization_subscribe_cache(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Path((organization, cache)): Path<(String, String)>,
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization: MOrganization =
        match get_organization_by_name(state.0.clone(), user.id, organization.clone()).await {
            Some(o) => o,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Organization not found".to_string(),
                    }),
                ))
            }
        };

    let cache: MCache = match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
        Some(c) => c,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ))
        }
    };

    let organization_cache = EOrganizationCache::find()
        .filter(
            Condition::all()
                .add(COrganizationCache::Organization.eq(organization.id))
                .add(COrganizationCache::Cache.eq(cache.id)),
        )
        .one(&state.db)
        .await
        .unwrap();

    if organization_cache.is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(BaseResponse {
                error: true,
                message: "Organization already subscribed to Cache".to_string(),
            }),
        ));
    }

    let aorganization_cache = AOrganizationCache {
        id: Set(Uuid::new_v4()),
        organization: Set(organization.id),
        cache: Set(cache.id),
    };

    aorganization_cache.insert(&state.db).await.unwrap();

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
) -> Result<Json<BaseResponse<String>>, (StatusCode, Json<BaseResponse<String>>)> {
    let organization: MOrganization =
        match get_organization_by_name(state.0.clone(), user.id, organization.clone()).await {
            Some(o) => o,
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(BaseResponse {
                        error: true,
                        message: "Organization not found".to_string(),
                    }),
                ))
            }
        };

    let cache: MCache = match get_cache_by_name(state.0.clone(), user.id, cache.clone()).await {
        Some(c) => c,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(BaseResponse {
                    error: true,
                    message: "Cache not found".to_string(),
                }),
            ))
        }
    };

    let organization_cache = EOrganizationCache::find()
        .filter(
            Condition::all()
                .add(COrganizationCache::Organization.eq(organization.id))
                .add(COrganizationCache::Cache.eq(cache.id)),
        )
        .one(&state.db)
        .await
        .unwrap();

    if let Some(organization_cache) = organization_cache {
        let aorganization_cache: AOrganizationCache = organization_cache.into();
        aorganization_cache.delete(&state.db).await.unwrap();
    } else {
        return Err((
            StatusCode::CONFLICT,
            Json(BaseResponse {
                error: true,
                message: "Organization not subscribed to Cache".to_string(),
            }),
        ));
    }

    let res = BaseResponse {
        error: false,
        message: "Cache unsubscribed".to_string(),
    };

    Ok(Json(res))
}
