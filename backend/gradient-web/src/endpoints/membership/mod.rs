/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod cache;
pub mod invitations;
pub mod members;
pub mod project;
pub mod roles;

pub use self::cache::CacheOwner;
pub use self::project::ProjectOwner;

use crate::error::{WebError, WebResult};
use crate::helpers::OptionExt;
use crate::lookup::HasResource;
use crate::permissions::{PermissionEntry, PermissionMask};
use crate::resource::Resource;
use crate::scope::{CacheRule, CacheScope, PathParams, ProjectRule, ProjectScope};
use axum::extract::FromRequestParts;
use chrono::NaiveDateTime;
use gradient_core::ServerState;
use gradient_db::lookup::by_ids;
use gradient_db::{DbContext, WebDb};
use gradient_notify::InviteScope;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;
use sea_orm::{
    ActiveModelBehavior, ActiveModelTrait, ConnectionTrait, EntityTrait, IntoActiveModel,
    PrimaryKeyTrait, RelationDef,
};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

pub trait OwnerId:
    Copy + PartialEq + Display + Into<sea_orm::Value> + Send + Sync + 'static
{
}

impl<T> OwnerId for T where
    T: Copy + PartialEq + Display + Into<sea_orm::Value> + Send + Sync + 'static
{
}

pub trait Table:
    EntityTrait<Model = <Self as Table>::Row, ActiveModel = <Self as Table>::Active>
{
    type Row: IntoActiveModel<Self::Active> + Send + Sync;
    type Active: ActiveModelTrait<Entity = Self> + ActiveModelBehavior + Send;
}

pub struct RoleFields<I> {
    pub id: RoleId,
    pub name: String,
    pub owner: Option<I>,
    pub permission: PermissionMask,
    pub managed: bool,
}

pub struct MemberFields {
    pub user: UserId,
    pub role: RoleId,
}

pub struct InvitationFields {
    pub user: UserId,
    pub role: RoleId,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
}

pub struct NewInvitation<I> {
    pub owner: I,
    pub user: UserId,
    pub role: RoleId,
    pub invited_by: UserId,
    pub token: String,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
}

pub trait RoleTable:
    Table + HasResource + EntityTrait<PrimaryKey: PrimaryKeyTrait<ValueType = RoleId>>
{
    type Id: OwnerId;
    type OwnerField: From<Option<Self::Id>> + Serialize + Send + 'static;

    fn id_column() -> Self::Column;
    fn name_column() -> Self::Column;
    fn owner_column() -> Self::Column;
    fn fields(row: &Self::Row) -> RoleFields<Self::Id>;
    fn new(id: RoleId, name: String, owner: Self::Id, permission: PermissionMask) -> Self::Row;
    fn rename(active: &mut Self::Active, name: String);
    fn set_permission(active: &mut Self::Active, permission: PermissionMask);
}

pub trait MemberTable: Table {
    type Id: OwnerId;

    fn owner_column() -> Self::Column;
    fn user_column() -> Self::Column;
    fn role_column() -> Self::Column;
    fn user_relation() -> RelationDef;
    fn fields(row: &Self::Row) -> MemberFields;
    fn new(owner: Self::Id, user: UserId, role: RoleId) -> Self::Row;
    fn assign_role(row: Self::Row, role: RoleId) -> Self::Active;
}

pub trait InvitationTable: Table {
    type Id: OwnerId;

    fn owner_column() -> Self::Column;
    fn user_column() -> Self::Column;
    fn user_relation() -> RelationDef;
    fn fields(row: &Self::Row) -> InvitationFields;
    fn new(invitation: NewInvitation<Self::Id>) -> Self::Row;
}

pub trait OwnerScope:
    FromRequestParts<Arc<ServerState>, Rejection = WebError> + Send + Sync
{
    type Id: OwnerId;

    fn id(&self) -> Self::Id;
    fn display_name(&self) -> &str;
    fn db(&self) -> &DbContext;
    fn params(&self) -> &PathParams;
}

pub trait ActingScope: OwnerScope {
    fn user(&self) -> &MUser;
}

impl<R: ProjectRule> OwnerScope for ProjectScope<R> {
    type Id = ProjectId;

    fn id(&self) -> ProjectId {
        self.project.id
    }

    fn display_name(&self) -> &str {
        &self.project.display_name
    }

    fn db(&self) -> &DbContext {
        &self.db
    }

    fn params(&self) -> &PathParams {
        &self.params
    }
}

impl<R: ProjectRule<User = MUser>> ActingScope for ProjectScope<R> {
    fn user(&self) -> &MUser {
        &self.user
    }
}

impl<R: CacheRule> OwnerScope for CacheScope<R> {
    type Id = CacheId;

    fn id(&self) -> CacheId {
        self.cache.id
    }

    fn display_name(&self) -> &str {
        &self.cache.display_name
    }

    fn db(&self) -> &DbContext {
        &self.db
    }

    fn params(&self) -> &PathParams {
        &self.params
    }
}

impl<R: CacheRule<User = MUser>> ActingScope for CacheScope<R> {
    fn user(&self) -> &MUser {
        &self.user
    }
}

#[derive(Clone, Copy)]
pub struct Actions {
    pub member_add: Action,
    pub member_role_change: Action,
    pub member_remove: Action,
    pub invitation_create: Action,
    pub invitation_revoke: Action,
    pub role_create: Action,
    pub role_update: Action,
    pub role_delete: Action,
}

pub type RoleRow<O> = <<O as Owner>::Roles as Table>::Row;
pub type MemberRow<O> = <<O as Owner>::Members as Table>::Row;

pub trait Owner: Send + Sync + 'static {
    type Id: OwnerId;
    type ListMembersScope: OwnerScope<Id = Self::Id>;
    type ManageMembersScope: ActingScope<Id = Self::Id>;
    type ListInvitationsScope: OwnerScope<Id = Self::Id>;
    type ReadRolesScope: OwnerScope<Id = Self::Id>;
    type ManageRolesScope: OwnerScope<Id = Self::Id>;
    type Members: MemberTable<Id = Self::Id>;
    type Invitations: InvitationTable<Id = Self::Id>;
    type Roles: RoleTable<Id = Self::Id>;

    const KIND: &'static str;
    const ID_KEY: &'static str;
    const RESOURCE: Resource;
    const MEMBERSHIP: Resource;
    const INVITE_SCOPE: InviteScope;
    const MEMBER_REMOVED: &'static str;
    const ACTIONS: Actions;

    fn event_owner(id: Self::Id) -> EventOwner;
    fn is_builtin_role(role: RoleId) -> bool;
    fn permission_names(mask: PermissionMask) -> Vec<&'static str>;
    fn available_permissions() -> Vec<PermissionEntry>;
    fn parse_permissions(wire: &[String]) -> WebResult<PermissionMask>;
    async fn role_granted_to_a_team(db: &impl ConnectionTrait, role: RoleId) -> WebResult<bool>;
    async fn lock_for_admin_change(db: &impl ConnectionTrait, owner: Self::Id) -> WebResult<()>;
    async fn ensure_member_removable(
        db: &impl ConnectionTrait,
        owner: Self::Id,
        member: &MemberRow<Self>,
    ) -> WebResult<()>;
}

pub async fn find_user_by_username(db: &WebDb, username: &str) -> WebResult<MUser> {
    gradient_db::users::by_username(db, username)
        .await?
        .or_not_found(Resource::User)
}

pub fn active<T: Table>(row: T::Row) -> T::Active {
    row.into_active_model()
}

pub async fn insert_unique_row<T: Table>(
    db: &WebDb,
    row: T::Row,
    taken: Resource,
) -> WebResult<T::Row> {
    gradient_db::rows::insert::<T, _>(db, row)
        .await
        .map_err(|e| WebError::from_db_err(e, taken))
}

pub async fn update_row<T: Table>(db: &WebDb, row: T::Active) -> WebResult<T::Row> {
    Ok(gradient_db::rows::update::<T, _>(db, row).await?)
}

pub async fn update_unique_row<T: Table>(
    db: &WebDb,
    row: T::Active,
    taken: Resource,
) -> WebResult<T::Row> {
    gradient_db::rows::update::<T, _>(db, row)
        .await
        .map_err(|e| WebError::from_db_err(e, taken))
}

pub async fn delete_row<T: Table>(db: &impl ConnectionTrait, row: T::Row) -> WebResult<()> {
    gradient_db::rows::delete::<T, _>(db, row).await?;
    Ok(())
}

pub async fn role_names<T: RoleTable>(
    db: &WebDb,
    role_ids: Vec<RoleId>,
) -> WebResult<HashMap<RoleId, String>> {
    Ok(by_ids::<T, _>(db, &role_ids)
        .await?
        .into_iter()
        .map(|(id, role)| (id, T::fields(&role).name))
        .collect())
}
