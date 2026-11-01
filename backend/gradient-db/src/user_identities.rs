/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

pub async fn user_by_identity<C: ConnectionTrait>(
    db: &C,
    issuer: &str,
    subject: &str,
) -> Result<Option<MUser>, DbErr> {
    let Some(identity) = EUserIdentity::find()
        .filter(CUserIdentity::Issuer.eq(issuer))
        .filter(CUserIdentity::Subject.eq(subject))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    EUser::find_by_id(identity.user).one(db).await
}

pub async fn of_user<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<Vec<MUserIdentity>, DbErr> {
    EUserIdentity::find()
        .filter(CUserIdentity::User.eq(user))
        .all(db)
        .await
}

pub async fn insert<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    issuer: &str,
    subject: &str,
) -> Result<MUserIdentity, DbErr> {
    let identity = AUserIdentity {
        id: Set(UserIdentityId::now_v7()),
        user: Set(user),
        issuer: Set(issuer.to_owned()),
        subject: Set(subject.to_owned()),
        created_at: Set(gradient_types::now()),
    };
    EUserIdentity::insert(identity)
        .exec_with_returning(db)
        .await
}

pub async fn delete<C: ConnectionTrait>(db: &C, user: UserId, issuer: &str) -> Result<u64, DbErr> {
    let result = EUserIdentity::delete_many()
        .filter(CUserIdentity::User.eq(user))
        .filter(CUserIdentity::Issuer.eq(issuer))
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}
