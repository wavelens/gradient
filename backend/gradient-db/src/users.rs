/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    PaginatorTrait, QueryFilter, QuerySelect, Select,
};

pub async fn by_username<C: ConnectionTrait>(
    db: &C,
    username: &str,
) -> Result<Option<MUser>, DbErr> {
    EUser::find()
        .filter(CUser::Username.eq(username))
        .one(db)
        .await
}

pub async fn by_username_or_email<C: ConnectionTrait>(
    db: &C,
    username: &str,
    email: &str,
) -> Result<Option<MUser>, DbErr> {
    EUser::find()
        .filter(
            Condition::any()
                .add(CUser::Username.eq(username))
                .add(CUser::Email.eq(email)),
        )
        .one(db)
        .await
}

pub async fn by_email<C: ConnectionTrait>(db: &C, email: &str) -> Result<Option<MUser>, DbErr> {
    EUser::find().filter(CUser::Email.eq(email)).one(db).await
}

pub async fn by_email_verification_token<C: ConnectionTrait>(
    db: &C,
    token: &str,
) -> Result<Option<MUser>, DbErr> {
    EUser::find()
        .filter(CUser::EmailVerificationToken.eq(token))
        .one(db)
        .await
}

pub async fn by_oidc_identity<C: ConnectionTrait>(
    db: &C,
    issuer: &str,
    subject: &str,
) -> Result<Option<MUser>, DbErr> {
    EUser::find()
        .filter(CUser::OidcIssuer.eq(issuer))
        .filter(CUser::OidcSubject.eq(subject))
        .one(db)
        .await
}

pub async fn search_by_username<C: ConnectionTrait>(
    db: &C,
    text: &str,
    limit: u64,
) -> Result<Vec<MUser>, DbErr> {
    EUser::find()
        .filter(CUser::Username.contains(text))
        .limit(limit)
        .all(db)
        .await
}

pub async fn mark_email_verified<C: ConnectionTrait>(db: &C, user: MUser) -> Result<MUser, DbErr> {
    let mut active = user.into_active_model();
    active.email_verified = Set(true);
    active.email_verification_token = Set(None);
    active.email_verification_token_expires = Set(None);
    active.update(db).await
}

pub async fn set_email_verification_token<C: ConnectionTrait>(
    db: &C,
    user: MUser,
    token: String,
    expires_at: NaiveDateTime,
) -> Result<MUser, DbErr> {
    let mut active = user.into_active_model();
    active.email_verification_token = Set(Some(token));
    active.email_verification_token_expires = Set(Some(expires_at));
    active.update(db).await
}

pub async fn stamp_last_login<C: ConnectionTrait>(
    db: &C,
    user: MUser,
    at: NaiveDateTime,
) -> Result<MUser, DbErr> {
    let mut active = user.into_active_model();
    active.last_login_at = Set(at);
    active.update(db).await
}

pub async fn with_ids<C: ConnectionTrait>(db: &C, ids: &[UserId]) -> Result<Vec<MUser>, DbErr> {
    crate::fetch_in_chunks(ids, |chunk| async move {
        EUser::find().filter(CUser::Id.is_in(chunk)).all(db).await
    })
    .await
}

pub enum ManagedUserFilter {
    Any,
    Username(String),
    ScimExternalId(String),
}

fn managed(filter: &ManagedUserFilter) -> Select<EUser> {
    let query = EUser::find().filter(CUser::Managed.eq(true));
    match filter {
        ManagedUserFilter::Any => query,
        ManagedUserFilter::Username(username) => {
            query.filter(CUser::Username.eq(username.as_str()))
        }
        ManagedUserFilter::ScimExternalId(external_id) => {
            query.filter(CUser::ScimExternalId.eq(external_id.as_str()))
        }
    }
}

pub async fn count_managed<C: ConnectionTrait>(
    db: &C,
    filter: &ManagedUserFilter,
) -> Result<u64, DbErr> {
    managed(filter).count(db).await
}

pub async fn page_of_managed<C: ConnectionTrait>(
    db: &C,
    filter: &ManagedUserFilter,
    per_page: u64,
    page: u64,
) -> Result<Vec<MUser>, DbErr> {
    managed(filter)
        .paginate(db, per_page)
        .fetch_page(page)
        .await
}

pub async fn managed_by_id<C: ConnectionTrait>(db: &C, id: UserId) -> Result<Option<MUser>, DbErr> {
    EUser::find_by_id(id)
        .filter(CUser::Managed.eq(true))
        .one(db)
        .await
}

pub async fn insert<C: ConnectionTrait>(db: &C, user: AUser) -> Result<MUser, DbErr> {
    user.insert(db).await
}

pub async fn update<C: ConnectionTrait>(db: &C, user: AUser) -> Result<MUser, DbErr> {
    user.update(db).await
}

pub async fn delete_by_id<C: ConnectionTrait>(db: &C, id: UserId) -> Result<(), DbErr> {
    EUser::delete_by_id(id).exec(db).await?;
    Ok(())
}
