/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::audit::{RequestInfo, record as audit_record};
use crate::error::{WebError, WebResult};
use crate::helpers::{OptionExt, ok_json};
use crate::invite_policy::invitation_expiry;
use crate::refusal::SharedRefusal;
use crate::resource::Resource;
use crate::scope::{CacheScope, Requires, RequiresUnmanaged, can};
use axum::Json;
use axum::extract::State;
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_db::lookup::by_ids;
use gradient_notify::{InvitationMail, InviteScope, generate_token};
use gradient_types::consts::BASE_CACHE_ROLE_ADMIN_ID;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, IntoActiveModel, JoinType,
    PaginatorTrait, QueryFilter, QuerySelect, RelationTrait,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMemberItem {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AddCacheMemberRequest {
    pub user: String,
    pub role: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RemoveCacheMemberRequest {
    pub user: String,
}

async fn find_user_by_username(db: &WebDb, username: &str) -> WebResult<MUser> {
    EUser::find()
        .filter(CUser::Username.eq(username))
        .one(db)
        .await?
        .or_not_found(Resource::User)
}

async fn find_cache_membership(
    db: &WebDb,
    cache_id: CacheId,
    user_id: UserId,
) -> WebResult<Option<MCacheUser>> {
    Ok(ECacheUser::find()
        .filter(
            Condition::all()
                .add(CCacheUser::Cache.eq(cache_id))
                .add(CCacheUser::User.eq(user_id)),
        )
        .one(db)
        .await?)
}

async fn find_cache_invitation(
    db: &WebDb,
    cache_id: CacheId,
    user_id: UserId,
) -> WebResult<Option<MCacheInvitation>> {
    Ok(ECacheInvitation::find()
        .filter(
            Condition::all()
                .add(CCacheInvitation::Cache.eq(cache_id))
                .add(CCacheInvitation::User.eq(user_id)),
        )
        .one(db)
        .await?)
}

pub async fn get_cache_members(
    CacheScope { cache, db, .. }: CacheScope<Requires<can::ViewCache>>,
) -> WebResult<Json<BaseResponse<Vec<CacheMemberItem>>>> {
    let cache_users = ECacheUser::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::cache_user::Relation::User.def(),
        )
        .select_also(gradient_entity::user::Entity)
        .filter(CCacheUser::Cache.eq(cache.id))
        .all(&db.web_db)
        .await?;

    let role_ids: Vec<RoleId> = cache_users.iter().map(|(cu, _)| cu.role).collect();
    let role_map: std::collections::HashMap<RoleId, String> =
        by_ids::<ECacheRole, _>(&db.web_db, &role_ids)
            .await?
            .into_iter()
            .map(|(id, r)| (id, r.name))
            .collect();

    let items: Vec<CacheMemberItem> = cache_users
        .iter()
        .map(|(cu, user)| CacheMemberItem {
            id: user
                .as_ref()
                .map(|u| u.username.clone())
                .unwrap_or_else(|| cu.user.to_string()),
            name: role_map
                .get(&cu.role)
                .cloned()
                .unwrap_or_else(|| cu.role.to_string()),
        })
        .collect();

    Ok(ok_json(items))
}

pub async fn post_cache_member(
    CacheScope {
        cache, user, db, ..
    }: CacheScope<RequiresUnmanaged<can::ManageCacheMembers>>,
    state: State<Arc<ServerState>>,
    info: RequestInfo,
    Json(body): Json<AddCacheMemberRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target_user = find_user_by_username(&state.web_db, &body.user).await?;

    if find_cache_membership(&state.web_db, cache.id, target_user.id)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(Resource::CacheMembership));
    }

    if find_cache_invitation(&state.web_db, cache.id, target_user.id)
        .await?
        .is_some()
    {
        return Err(WebError::already_exists(Resource::Invitation));
    }

    let role = ECacheRole::find()
        .filter(
            Condition::all()
                .add(CCacheRole::Name.eq(body.role.clone()))
                .add(
                    Condition::any()
                        .add(CCacheRole::Cache.eq(cache.id))
                        .add(CCacheRole::Cache.is_null()),
                ),
        )
        .one(&state.web_db)
        .await?
        .or_not_found(Resource::Role)?;

    if user.superuser {
        MCacheUser {
            id: CacheUserId::now_v7(),
            cache: cache.id,
            user: target_user.id,
            role: role.id,
        }
        .into_active_model()
        .insert(&state.web_db)
        .await?;

        audit_record(
            &db,
            Some(user.id),
            Action::CacheMemberCreate,
            EventOwner {
                cache: Some(cache.id),
                ..Default::default()
            },
            &info,
            Some(serde_json::json!({
                "cache_id": cache.id.to_string(),
                "target_user_id": target_user.id.to_string(),
                "role": role.name,
            })),
        )
        .await;

        return Ok(ok_json("User added".to_string()));
    }

    let now = gradient_types::now();
    let token = generate_token();

    MCacheInvitation {
        id: CacheInvitationId::now_v7(),
        cache: cache.id,
        user: target_user.id,
        role: role.id,
        invited_by: user.id,
        token: token.clone(),
        created_at: now,
        expires_at: invitation_expiry(now),
    }
    .into_active_model()
    .insert(&state.web_db)
    .await?;

    if state.email.is_enabled()
        && let Err(e) = state
            .email
            .send_invitation_email(
                &target_user.email,
                &target_user.name,
                &InvitationMail {
                    scope: InviteScope::Cache,
                    scope_display_name: &cache.display_name,
                    role: &role.name,
                    inviter: &user.username,
                    accept_url: format!(
                        "{}/settings/invites?token={}",
                        state.config.server.serve_url, token
                    ),
                },
            )
            .await
    {
        tracing::warn!(error = %e, "Failed to send cache invitation email");
    }

    audit_record(
        &db,
        Some(user.id),
        Action::CacheInvitationCreate,
        EventOwner {
            cache: Some(cache.id),
            ..Default::default()
        },
        &info,
        Some(serde_json::json!({
            "cache_id": cache.id.to_string(),
            "target_user_id": target_user.id.to_string(),
            "role": role.name,
        })),
    )
    .await;

    Ok(ok_json("Invitation sent".to_string()))
}

pub async fn patch_cache_member(
    CacheScope {
        cache, user, db, ..
    }: CacheScope<RequiresUnmanaged<can::ManageCacheMembers>>,
    info: RequestInfo,
    Json(body): Json<AddCacheMemberRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target_user = find_user_by_username(&db.web_db, &body.user).await?;

    let membership = find_cache_membership(&db.web_db, cache.id, target_user.id)
        .await?
        .ok_or(SharedRefusal::NotAMember(Resource::Cache))?;

    let previous_role_id = membership.role;
    let role = ECacheRole::find()
        .filter(
            Condition::all()
                .add(CCacheRole::Name.eq(body.role.clone()))
                .add(
                    Condition::any()
                        .add(CCacheRole::Cache.eq(cache.id))
                        .add(CCacheRole::Cache.is_null()),
                ),
        )
        .one(&db.web_db)
        .await?
        .or_not_found(Resource::Role)?;

    let mut active: ACacheUser = membership.into();
    active.role = Set(role.id);
    active.update(&db.web_db).await?;

    audit_record(
        &db,
        Some(user.id),
        Action::CacheMemberUpdate,
        EventOwner {
            cache: Some(cache.id),
            ..Default::default()
        },
        &info,
        Some(serde_json::json!({
            "cache_id": cache.id.to_string(),
            "target_user_id": target_user.id.to_string(),
            "previous_role_id": previous_role_id.to_string(),
            "new_role": role.name,
        })),
    )
    .await;

    Ok(ok_json("User role updated".to_string()))
}

pub async fn delete_cache_member(
    CacheScope {
        cache, user, db, ..
    }: CacheScope<RequiresUnmanaged<can::ManageCacheMembers>>,
    info: RequestInfo,
    Json(body): Json<RemoveCacheMemberRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target_user = find_user_by_username(&db.web_db, &body.user).await?;

    let membership = find_cache_membership(&db.web_db, cache.id, target_user.id)
        .await?
        .ok_or(SharedRefusal::NotAMember(Resource::Cache))?;

    if membership.role == BASE_CACHE_ROLE_ADMIN_ID {
        let admin_count = ECacheUser::find()
            .filter(CCacheUser::Cache.eq(cache.id))
            .filter(CCacheUser::Role.eq(BASE_CACHE_ROLE_ADMIN_ID))
            .count(&db.web_db)
            .await?;
        if admin_count <= 1 {
            return Err(SharedRefusal::LastAdmin(Resource::Cache).into());
        }
    }

    let active: ACacheUser = membership.into();
    active.delete(&db.web_db).await?;

    audit_record(
        &db,
        Some(user.id),
        Action::CacheMemberDelete,
        EventOwner {
            cache: Some(cache.id),
            ..Default::default()
        },
        &info,
        Some(serde_json::json!({
            "cache_id": cache.id.to_string(),
            "target_user_id": target_user.id.to_string(),
        })),
    )
    .await;

    Ok(ok_json("User removed".to_string()))
}
