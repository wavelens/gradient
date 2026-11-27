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
    Order, QueryFilter, QueryOrder, Select,
};

pub fn usable_sessions_at(now: NaiveDateTime) -> Select<ESession> {
    let session_created_within_max_total_age = [false, true].into_iter().fold(
        Condition::any(),
        |condition, is_cli_device_sign_in_session| {
            condition.add(
                Condition::all()
                    .add(CSession::IsCliDeviceSignInSession.eq(is_cli_device_sign_in_session))
                    .add(CSession::CreatedAt.gte(
                        now - MSession::maximum_total_session_age(is_cli_device_sign_in_session),
                    )),
            )
        },
    );
    ESession::find()
        .filter(CSession::RevokedAt.is_null())
        .filter(CSession::ExpiresAt.gte(now))
        .filter(session_created_within_max_total_age)
}

pub fn active_sessions_of_user(
    user: UserId,
    current_session: Option<SessionId>,
    now: NaiveDateTime,
) -> Select<ESession> {
    let query = usable_sessions_at(now).filter(CSession::UserId.eq(user));
    let query = match current_session {
        Some(current) => query.order_by(CSession::Id.eq(current), Order::Desc),
        None => query,
    };
    query
        .order_by_desc(CSession::LastUsedAt)
        .order_by_asc(CSession::Id)
}

pub async fn usable_browser_session_ip_addresses_of_user<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    now: NaiveDateTime,
) -> Result<Vec<String>, DbErr> {
    Ok(usable_sessions_at(now)
        .filter(CSession::UserId.eq(user))
        .all(db)
        .await?
        .into_iter()
        .filter(|session| !session.is_cli_device_sign_in_session)
        .filter_map(|session| session.ip)
        .collect())
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
    let expires_at = session.expiry_after_renewal_at(at);
    let mut active = session.into_active_model();
    active.expires_at = Set(expires_at);
    active.last_used_at = Set(at);
    active.update(db).await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn insert_session_signed_in_ago(
        connection: &sea_orm::DatabaseConnection,
        user_id: UserId,
        age: chrono::TimeDelta,
        is_cli_device_sign_in_session: bool,
        now: NaiveDateTime,
    ) -> MSession {
        let session = MSession {
            id: SessionId::now_v7(),
            user_id,
            created_at: now - age,
            expires_at: now + chrono::TimeDelta::hours(1),
            last_used_at: now,
            remember_me: true,
            is_cli_device_sign_in_session,
            ..Default::default()
        };
        crate::rows::insert::<ESession, _>(connection, session)
            .await
            .expect("insert session")
    }

    #[tokio::test]
    async fn renewal_never_extends_past_absolute_lifetime() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        let user = crate::rows::insert::<EUser, _>(&connection, gradient_fixtures::rows::user())
            .await
            .expect("insert user");
        let now = chrono::SubsecRound::trunc_subsecs(gradient_types::now(), 6);
        let browser = insert_session_signed_in_ago(
            &connection,
            user.id,
            chrono::TimeDelta::days(90) - chrono::TimeDelta::hours(1),
            false,
            now,
        )
        .await;
        let cli_device = insert_session_signed_in_ago(
            &connection,
            user.id,
            chrono::TimeDelta::days(30) - chrono::TimeDelta::hours(1),
            true,
            now,
        )
        .await;

        let browser_end = browser.created_at + chrono::TimeDelta::days(90);
        let cli_device_end = cli_device.created_at + chrono::TimeDelta::days(30);
        extend_expiry(&connection, browser.clone(), now)
            .await
            .expect("extend browser session");
        extend_expiry(&connection, cli_device.clone(), now)
            .await
            .expect("extend CLI device session");

        let stored_browser = db
            .find::<ESession>(browser.id)
            .await
            .expect("browser session row");
        let stored_cli_device = db
            .find::<ESession>(cli_device.id)
            .await
            .expect("CLI device session row");
        assert_eq!(stored_browser.expires_at, browser_end);
        assert_eq!(stored_cli_device.expires_at, cli_device_end);
        db.finish().await;
    }

    #[tokio::test]
    async fn postgres_session_list_of_user_contains_only_sessions_usable_now() {
        use sea_orm::PaginatorTrait;

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        let user = crate::rows::insert::<EUser, _>(&connection, gradient_fixtures::rows::user())
            .await
            .expect("insert user");
        let now = chrono::SubsecRound::trunc_subsecs(gradient_types::now(), 6);
        let day = chrono::TimeDelta::days(1);
        let browser = insert_session_signed_in_ago(&connection, user.id, day, false, now).await;
        let cli_device =
            insert_session_signed_in_ago(&connection, user.id, day * 29, true, now).await;
        let mut sessions = vec![browser.clone(), cli_device.clone()];
        sessions
            .push(insert_session_signed_in_ago(&connection, user.id, day * 91, false, now).await);
        sessions
            .push(insert_session_signed_in_ago(&connection, user.id, day * 31, true, now).await);
        let revoked = insert_session_signed_in_ago(&connection, user.id, day, false, now).await;
        sessions.push(revoke(&connection, revoked, now).await.expect("revoke"));
        let mut expired = insert_session_signed_in_ago(&connection, user.id, day, false, now)
            .await
            .into_active_model();
        expired.expires_at = Set(now - chrono::TimeDelta::seconds(1));
        sessions.push(expired.update(&connection).await.expect("expire"));

        let listed: std::collections::BTreeSet<SessionId> =
            active_sessions_of_user(user.id, None, now)
                .all(&connection)
                .await
                .expect("list sessions")
                .into_iter()
                .map(|session| session.id)
                .collect();
        let usable: std::collections::BTreeSet<SessionId> = sessions
            .iter()
            .filter(|session| session.is_usable_at(now))
            .map(|session| session.id)
            .collect();
        assert_eq!(listed, usable);
        assert_eq!(listed, [browser.id, cli_device.id].into_iter().collect());
        let colleague = crate::rows::insert::<EUser, _>(
            &connection,
            MUser {
                id: UserId::now_v7(),
                username: "colleague".into(),
                name: "colleague".into(),
                email: "colleague@example.com".into(),
                ..gradient_fixtures::rows::user()
            },
        )
        .await
        .expect("insert second user");
        insert_session_signed_in_ago(&connection, colleague.id, day, false, now).await;
        let mut older_use = browser.clone().into_active_model();
        older_use.last_used_at = Set(now - chrono::TimeDelta::hours(1));
        older_use.update(&connection).await.expect("older use");
        let ordered = |current| {
            let connection = connection.clone();
            async move {
                active_sessions_of_user(user.id, current, now)
                    .all(&connection)
                    .await
                    .expect("list sessions")
                    .into_iter()
                    .map(|session| session.id)
                    .collect::<Vec<_>>()
            }
        };
        assert_eq!(ordered(Some(browser.id)).await, [browser.id, cli_device.id]);
        assert_eq!(ordered(None).await, [cli_device.id, browser.id]);
        assert_eq!(
            usable_sessions_at(now)
                .count(&connection)
                .await
                .expect("count"),
            3
        );
        db.finish().await;
    }
}
