/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel, QueryFilter,
};

pub async fn unexpired_by_user_code<C: ConnectionTrait>(
    db: &C,
    user_code: &str,
    now: NaiveDateTime,
) -> Result<Option<MCliDeviceAuthorization>, DbErr> {
    ECliDeviceAuthorization::find()
        .filter(CCliDeviceAuthorization::UserCode.eq(user_code))
        .filter(CCliDeviceAuthorization::ExpiresAt.gt(now))
        .one(db)
        .await
}

pub async fn by_device_code_hash<C: ConnectionTrait>(
    db: &C,
    device_code_hash: &str,
) -> Result<Option<MCliDeviceAuthorization>, DbErr> {
    ECliDeviceAuthorization::find()
        .filter(CCliDeviceAuthorization::DeviceCodeHash.eq(device_code_hash))
        .one(db)
        .await
}

pub async fn by_user_code<C: ConnectionTrait>(
    db: &C,
    user_code: &str,
) -> Result<Option<MCliDeviceAuthorization>, DbErr> {
    ECliDeviceAuthorization::find()
        .filter(CCliDeviceAuthorization::UserCode.eq(user_code))
        .one(db)
        .await
}

fn still_pending(at: NaiveDateTime) -> Condition {
    Condition::all()
        .add(CCliDeviceAuthorization::AuthorizedAt.is_null())
        .add(CCliDeviceAuthorization::DeniedAt.is_null())
        .add(CCliDeviceAuthorization::ExpiresAt.gte(at))
}

pub async fn authorize<C: ConnectionTrait>(
    db: &C,
    authorization: MCliDeviceAuthorization,
    user: UserId,
    approving_session: SessionId,
    at: NaiveDateTime,
) -> Result<Option<MCliDeviceAuthorization>, DbErr> {
    let mut active = authorization.into_active_model();
    active.user_id = Set(Some(user));
    active.approving_session_id = Set(Some(approving_session));
    active.authorized_at = Set(Some(at));
    crate::rows::update_where::<ECliDeviceAuthorization, _>(db, active, still_pending(at)).await
}

pub async fn delete_authorized_unexpired_cli_device_authorization_by_device_code_hash<
    C: ConnectionTrait,
>(
    db: &C,
    device_code_hash: &str,
    at: NaiveDateTime,
) -> Result<Option<MCliDeviceAuthorization>, DbErr> {
    let mut deleted = ECliDeviceAuthorization::delete_many()
        .filter(CCliDeviceAuthorization::DeviceCodeHash.eq(device_code_hash))
        .filter(CCliDeviceAuthorization::AuthorizedAt.is_not_null())
        .filter(CCliDeviceAuthorization::DeniedAt.is_null())
        .filter(CCliDeviceAuthorization::ApprovingSessionId.is_not_null())
        .filter(CCliDeviceAuthorization::ExpiresAt.gte(at))
        .exec_with_returning(db)
        .await?;
    Ok(deleted.pop())
}

pub async fn deny<C: ConnectionTrait>(
    db: &C,
    authorization: MCliDeviceAuthorization,
    at: NaiveDateTime,
) -> Result<Option<MCliDeviceAuthorization>, DbErr> {
    let mut active = authorization.into_active_model();
    active.denied_at = Set(Some(at));
    crate::rows::update_where::<ECliDeviceAuthorization, _>(db, active, still_pending(at)).await
}

crate::sql! {
    pub CLEAR_EXPIRED_CLI_DEVICE_AUTHORIZATION_TOKENS = "UPDATE cli_device_authorization \
        SET token = NULL \
        WHERE token IS NOT NULL AND expires_at < $1",
        params = [Now],
        tier = Sweep;
}

pub async fn clear_expired_cli_device_authorization_tokens<C: ConnectionTrait>(
    db: &C,
    now: NaiveDateTime,
) -> Result<u64, DbErr> {
    Ok(db
        .execute_raw(CLEAR_EXPIRED_CLI_DEVICE_AUTHORIZATION_TOKENS.bind([now.into()]))
        .await?
        .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;
    use gradient_migration::{Migrator, MigratorTrait};
    use sea_orm_migration::SchemaManager;

    const COLUMNS_OF_SESSION_AND_DEVICE_AUTHORIZATION: &str = "SELECT table_name::text || '.' || column_name::text \
         FROM information_schema.columns \
         WHERE (table_name = 'session' AND column_name = 'is_cli_device_sign_in_session') \
            OR (table_name = 'cli_device_authorization' \
                AND column_name IN ('token', 'approving_session_id')) \
         ORDER BY 1";

    #[tokio::test]
    async fn postgres_migration_up_sets_cli_device_sign_in_session_false_deletes_cli_device_authorizations_and_adds_approving_session_id_then_down_removes_flag_and_column_keeping_token()
     {
        let db = TestDatabase::create().await;
        let connection = db.connection();
        let user = crate::rows::insert::<EUser, _>(&connection, rows::user())
            .await
            .expect("The user row is now in the database.");
        let migration = Migrator::migrations()
            .into_iter()
            .find(|m| m.name().starts_with("m20270101_000000_"))
            .expect("register a migration to replace the device token with the approving session.");
        let manager = SchemaManager::new(&connection);
        migration.down(&manager).await.expect("migration rollback");
        db.execute(&format!(
            "INSERT INTO session (id, user_id, created_at, expires_at, last_used_at, remember_me) \
             VALUES (gen_random_uuid(), '{}', now(), now() + interval '1 day', now(), true)",
            user.id
        ))
        .await;
        db.execute(
            "INSERT INTO cli_device_authorization (id, device_code_hash, user_code, token, \
             created_at, expires_at) VALUES \
             (gen_random_uuid(), 'hash', 'ABCD-EFGH', 'stored-jwt', now(), now() + interval '10 minutes')",
        )
        .await;

        migration.up(&manager).await.expect("The migration ran");
        let flags = db
            .texts("SELECT is_cli_device_sign_in_session::text FROM session")
            .await;
        let device_rows = db
            .texts("SELECT id::text FROM cli_device_authorization")
            .await;
        let columns_after_up = db.texts(COLUMNS_OF_SESSION_AND_DEVICE_AUTHORIZATION).await;
        migration
            .down(&manager)
            .await
            .expect("A second run of the migration down step.");
        let columns_after_down = db.texts(COLUMNS_OF_SESSION_AND_DEVICE_AUTHORIZATION).await;
        migration
            .up(&manager)
            .await
            .expect("The migration ran again.");

        assert_eq!(flags, vec!["false".to_string()]);
        assert!(device_rows.is_empty(), "{device_rows:?}");
        assert_eq!(
            columns_after_up,
            vec![
                "cli_device_authorization.approving_session_id".to_string(),
                "cli_device_authorization.token".to_string(),
                "session.is_cli_device_sign_in_session".to_string(),
            ]
        );
        assert_eq!(
            columns_after_down,
            vec!["cli_device_authorization.token".to_string()]
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn expiry_sweep_nulls_token_on_expired_cli_device_authorization_from_old_server_and_keeps_token_on_unexpired_row()
     {
        let db = TestDatabase::create().await;
        let connection = db.connection();
        db.execute(
            "INSERT INTO cli_device_authorization (id, device_code_hash, user_code, token, \
             created_at, expires_at) VALUES \
             (gen_random_uuid(), 'expired-hash', 'EXPI-RED0', 'expired-jwt', \
              now() - interval '20 minutes', now() - interval '10 minutes'), \
             (gen_random_uuid(), 'pending-hash', 'PEND-ING0', 'pending-jwt', \
              now(), now() + interval '10 minutes')",
        )
        .await;

        clear_expired_cli_device_authorization_tokens(&connection, gradient_types::now())
            .await
            .expect("The expiry sweep ran.");

        assert_eq!(
            db.texts(
                "SELECT user_code || ' ' || coalesce(token, 'NULL') \
                 FROM cli_device_authorization ORDER BY user_code"
            )
            .await,
            vec![
                "EXPI-RED0 NULL".to_string(),
                "PEND-ING0 pending-jwt".to_string()
            ],
            "The sweep can clear the token on expired rows. The sweep can keep the token on rows that aren't expired."
        );
        db.finish().await;
    }
}
