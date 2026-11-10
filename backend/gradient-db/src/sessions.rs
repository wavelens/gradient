/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, Order,
    QueryFilter, QueryOrder, Select,
};

pub fn active_sessions_of_user(
    user: UserId,
    current_session: Option<SessionId>,
    now: NaiveDateTime,
) -> Select<ESession> {
    let query = ESession::find()
        .filter(CSession::UserId.eq(user))
        .filter(CSession::RevokedAt.is_null())
        .filter(CSession::ExpiresAt.gte(now));
    let query = match current_session {
        Some(current) => query.order_by(CSession::Id.eq(current), Order::Desc),
        None => query,
    };
    query
        .order_by_desc(CSession::LastUsedAt)
        .order_by_asc(CSession::Id)
}

pub async fn revoke<C: ConnectionTrait>(
    db: &C,
    session: MSession,
    at: NaiveDateTime,
) -> Result<MSession, DbErr> {
    let mut active = session.into_active_model();
    active.revoked_at = Set(Some(at));
    active.update(db).await
}

pub async fn stamp_last_used<C: ConnectionTrait>(
    db: &C,
    session: MSession,
    at: NaiveDateTime,
) -> Result<MSession, DbErr> {
    let mut active = session.into_active_model();
    active.last_used_at = Set(at);
    active.update(db).await
}

pub async fn extend_expiry<C: ConnectionTrait>(
    db: &C,
    session: MSession,
    at: NaiveDateTime,
) -> Result<MSession, DbErr> {
    let expires_at = at + MSession::sign_in_session_valid_duration(session.remember_me);
    let mut active = session.into_active_model();
    active.expires_at = Set(expires_at);
    active.last_used_at = Set(at);
    active.update(db).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, QueryTrait};

    #[test]
    fn active_sessions_query_excludes_revoked_and_expired_and_orders_own_session_then_last_use() {
        let sql = active_sessions_of_user(
            UserId::now_v7(),
            Some(SessionId::now_v7()),
            gradient_types::now(),
        )
        .build(DatabaseBackend::Postgres)
        .to_string();
        assert!(sql.contains(r#""revoked_at" IS NULL"#), "sql = {sql}");
        assert!(sql.contains(r#""expires_at" >="#), "sql = {sql}");
        let (_, order) = sql.split_once("ORDER BY").expect("an ORDER BY clause");
        let own = order.find(r#""id" = "#).expect("own session ordered first");
        let last_used = order.find("last_used_at").expect("ordered by last use");
        assert!(own < last_used, "order = {order}");
    }
}
