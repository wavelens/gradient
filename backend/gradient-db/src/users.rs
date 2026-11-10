/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::sea_query::{Expr, ExprTrait, Func, SimpleExpr};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Select,
};

pub async fn by_username<C: ConnectionTrait>(
    db: &C,
    username: &str,
) -> Result<Option<MUser>, DbErr> {
    EUser::find()
        .filter(username_equals_ignoring_case_sql_filter(username))
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
                .add(username_equals_ignoring_case_sql_filter(username))
                .add(email_equals_ignoring_case_sql_filter(email)),
        )
        .one(db)
        .await
}

fn username_equals_ignoring_case_sql_filter(username: &str) -> SimpleExpr {
    Expr::expr(Func::lower(Expr::col((EUser::default(), CUser::Username))))
        .eq(username.to_lowercase())
}

fn email_equals_ignoring_case_sql_filter(email: &str) -> SimpleExpr {
    Expr::expr(Func::lower(Expr::col((EUser::default(), CUser::Email)))).eq(email.to_lowercase())
}

pub async fn by_email<C: ConnectionTrait>(db: &C, email: &str) -> Result<Option<MUser>, DbErr> {
    EUser::find()
        .filter(email_equals_ignoring_case_sql_filter(email))
        .one(db)
        .await
}

pub use self::by_email as by_email_ignoring_case;

pub async fn by_email_verification_token<C: ConnectionTrait>(
    db: &C,
    token: &str,
) -> Result<Option<MUser>, DbErr> {
    EUser::find()
        .filter(CUser::EmailVerificationToken.eq(token))
        .one(db)
        .await
}

pub async fn search_by_username<C: ConnectionTrait>(
    db: &C,
    text: &str,
    limit: u64,
) -> Result<Vec<MUser>, DbErr> {
    use sea_orm::sea_query::extension::postgres::PgExpr;

    EUser::find()
        .filter(PgExpr::ilike(
            Expr::col(CUser::Username),
            crate::dashboard::ilike_contains(text),
        ))
        .order_by_asc(CUser::Username)
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

pub async fn list_managed_users<C: ConnectionTrait>(
    db: &C,
    filter: &ManagedUserFilter,
    offset: u64,
    limit: u64,
) -> Result<Vec<MUser>, DbErr> {
    managed(filter)
        .order_by_asc(CUser::Id)
        .offset(offset)
        .limit(limit)
        .all(db)
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

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{mock_db, raw_statements};

    #[tokio::test]
    async fn email_lookup_for_claims_ignores_case() {
        let db = mock_db()
            .append_query_results([Vec::<MUser>::new()])
            .into_connection();

        by_email_ignoring_case(&db, "Alice@Example.com")
            .await
            .expect("query");

        let statements = raw_statements(db.into_transaction_log());
        let [select] = statements.as_slice() else {
            panic!("expected one statement, got {statements:?}");
        };
        assert!(
            select.sql.contains("LOWER(\"user\".\"email\")"),
            "{}",
            select.sql
        );
        let values = format!("{:?}", select.values);
        assert!(values.contains("\"alice@example.com\""), "{values}");
    }

    #[tokio::test]
    async fn search_by_username_is_case_insensitive_escapes_wildcards_and_orders_by_username() {
        let db = mock_db()
            .append_query_results([Vec::<MUser>::new()])
            .into_connection();

        search_by_username(&db, "a_b%", 10).await.expect("query");

        let statements = raw_statements(db.into_transaction_log());
        let [select] = statements.as_slice() else {
            panic!("expected one statement, got {statements:?}");
        };
        assert!(select.sql.contains("ILIKE"), "{}", select.sql);
        assert!(
            select.sql.contains("ORDER BY \"user\".\"username\" ASC"),
            "{}",
            select.sql
        );
        let values = format!("{:?}", select.values);
        assert!(values.contains(r#""%a\\_b\\%%""#), "{values}");
    }

    #[tokio::test]
    async fn usernames_differing_only_in_case_are_one_name() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let account = |username: &str, email: &str| MUser {
            id: UserId::now_v7(),
            username: username.into(),
            name: username.into(),
            email: email.into(),
            active: true,
            ..Default::default()
        };
        crate::rows::insert::<EUser, _>(&db.connection(), account("alice", "a@example.com"))
            .await
            .expect("the first account is written");

        let found = by_username(&db.connection(), "ALICE")
            .await
            .expect("the lookup runs")
            .map(|user| user.username);
        let second =
            crate::rows::insert::<EUser, _>(&db.connection(), account("Alice", "b@example.com"))
                .await;

        assert_eq!(found.as_deref(), Some("alice"), "the lookup ignores case");
        assert!(second.is_err(), "the index refuses a case variant");
        db.finish().await;
    }

    #[tokio::test]
    async fn username_index_migration_renames_case_duplicates() {
        use sea_orm_migration::MigratorTrait;

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        let migrations = gradient_migration::Migrator::migrations();
        let case_insensitive_username_unique_index_migration_position = migrations
            .iter()
            .position(|m| m.name() == "m20261124_000000_case_insensitive_username_unique_index")
            .expect("register the migration for the index on user names");
        let migration_count_to_revert_before_case_insensitive_username_unique_index =
            u32::try_from(
                migrations.len() - case_insensitive_username_unique_index_migration_position,
            )
            .expect("the count fits in u32");
        gradient_migration::Migrator::down(
            &connection,
            Some(migration_count_to_revert_before_case_insensitive_username_unique_index),
        )
        .await
        .expect("the migrations after the case-sensitive username index revert");
        let start = chrono::DateTime::UNIX_EPOCH.naive_utc();
        let account = |username: &str, minute: i64, managed: bool| MUser {
            id: UserId::now_v7(),
            username: username.into(),
            name: username.into(),
            email: format!("{username}@example.com"),
            active: true,
            managed,
            created_at: start + chrono::Duration::minutes(minute),
            ..Default::default()
        };
        let accounts = [
            account("alice", 0, false),
            account("Alice", 1, false),
            account("ALICE", 2, false),
            account("bob", 0, false),
            account("Bob", 1, true),
            account("carol", 0, false),
            account("CAROL-2", 0, false),
            account("Carol", 1, false),
        ];
        for user in &accounts {
            crate::rows::insert::<EUser, _>(&connection, user.clone())
                .await
                .expect("case variants are allowed before the migration");
        }

        gradient_migration::Migrator::up(&connection, None)
            .await
            .expect("the migration runs on case duplicates");

        let mut renamed = Vec::new();
        for user in &accounts {
            let row = db
                .find::<EUser>(user.id)
                .await
                .expect("the account remains");
            renamed.push((user.username.clone(), row.username));
        }
        let pairs = |list: &[(&str, &str)]| -> Vec<(String, String)> {
            list.iter()
                .map(|(old, new)| (old.to_string(), new.to_string()))
                .collect()
        };
        assert_eq!(
            renamed,
            pairs(&[
                ("alice", "alice"),
                ("Alice", "Alice-2"),
                ("ALICE", "ALICE-3"),
                ("bob", "bob-2"),
                ("Bob", "Bob"),
                ("carol", "carol"),
                ("CAROL-2", "CAROL-2"),
                ("Carol", "Carol-3"),
            ])
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn emails_differing_only_in_case_are_one_account() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let account = |username: &str, email: &str| MUser {
            id: UserId::now_v7(),
            username: username.into(),
            name: username.into(),
            email: email.into(),
            active: true,
            ..Default::default()
        };
        crate::rows::insert::<EUser, _>(&db.connection(), account("alice", "alice@example.com"))
            .await
            .expect("the first account is written");
        crate::rows::insert::<EUser, _>(&db.connection(), account("bob", ""))
            .await
            .expect("an account without email is written");
        crate::rows::insert::<EUser, _>(&db.connection(), account("carol", ""))
            .await
            .expect("a second account without email is written");

        let found = by_email(&db.connection(), "Alice@Example.com")
            .await
            .expect("the lookup runs")
            .map(|user| user.username);
        let found_by_either = by_username_or_email(&db.connection(), "nobody", "ALICE@example.com")
            .await
            .expect("the lookup runs")
            .map(|user| user.username);
        let second = crate::rows::insert::<EUser, _>(
            &db.connection(),
            account("mallory", "Alice@example.COM"),
        )
        .await;

        assert_eq!(found.as_deref(), Some("alice"), "the lookup ignores case");
        assert_eq!(found_by_either.as_deref(), Some("alice"));
        assert!(second.is_err(), "the index refuses a case variant");
        db.finish().await;
    }

    #[tokio::test]
    async fn email_index_migration_clears_case_duplicate_emails_of_newer_accounts() {
        use sea_orm_migration::MigratorTrait;

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        let migrations = gradient_migration::Migrator::migrations();
        let position = migrations
            .iter()
            .position(|m| m.name() == "m20261201_000001_case_insensitive_email_unique_index")
            .expect("register the migration for the index on emails");
        gradient_migration::Migrator::down(
            &connection,
            Some(u32::try_from(migrations.len() - position).expect("the count fits in u32")),
        )
        .await
        .expect("the migrations after the email index revert");
        let start = chrono::DateTime::UNIX_EPOCH.naive_utc();
        let account =
            |username: &str, email: &str, minute: i64, managed: bool, verified: bool| MUser {
                id: UserId::now_v7(),
                username: username.into(),
                name: username.into(),
                email: email.into(),
                active: true,
                managed,
                email_verified: verified,
                created_at: start + chrono::Duration::minutes(minute),
                ..Default::default()
            };
        let accounts = [
            account("alice", "alice@example.com", 0, false, true),
            account("alice2", "Alice@example.com", 1, false, true),
            account("bob", "bob@example.com", 0, false, false),
            account("bob2", "BOB@example.com", 1, false, true),
            account("carol", "carol@example.com", 0, false, true),
            account("carol2", "Carol@example.com", 1, true, true),
            account("dave", "", 0, false, false),
            account("erin", "", 1, false, false),
        ];
        for user in &accounts {
            crate::rows::insert::<EUser, _>(&connection, user.clone())
                .await
                .expect("case variants are allowed before the migration");
        }

        gradient_migration::Migrator::up(&connection, None)
            .await
            .expect("the migration runs on case duplicates");

        let mut emails = Vec::new();
        for user in &accounts {
            let row = db
                .find::<EUser>(user.id)
                .await
                .expect("the account remains");
            emails.push((user.username.clone(), row.email, row.email_verified));
        }
        let expected: Vec<(String, String, bool)> = [
            ("alice", "alice@example.com", true),
            ("alice2", "", false),
            ("bob", "", false),
            ("bob2", "BOB@example.com", true),
            ("carol", "", false),
            ("carol2", "Carol@example.com", true),
            ("dave", "", false),
            ("erin", "", false),
        ]
        .iter()
        .map(|(name, email, verified)| (name.to_string(), email.to_string(), *verified))
        .collect();
        assert_eq!(emails, expected);
        db.finish().await;
    }

    #[tokio::test]
    async fn test_delete_user_with_project_fails_and_keeps_project() {
        use gradient_fixtures::ids::{project_id, user_id};
        use gradient_fixtures::rows;

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        crate::rows::insert::<EUser, _>(&connection, rows::user())
            .await
            .expect("the creator is written");
        crate::rows::insert::<EProject, _>(&connection, rows::project())
            .await
            .expect("the project is written");

        let deleted = delete_by_id(&connection, user_id()).await;

        assert!(deleted.is_err(), "the creator of a project stays");
        assert!(db.find::<EProject>(project_id()).await.is_some());
        db.finish().await;
    }

    #[tokio::test]
    async fn deleting_project_removes_custom_roles_and_member_rows_using_them() {
        use gradient_fixtures::ids::project_id;
        use gradient_fixtures::rows;

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        let member = MUser {
            id: UserId::now_v7(),
            username: "member".into(),
            email: "member@example.com".into(),
            ..rows::user()
        };
        let role = gradient_entity::role::Model {
            id: RoleId::now_v7(),
            name: "Reviewer".into(),
            project: Some(project_id()),
            permission: 1,
            managed: false,
        };
        crate::rows::insert::<EUser, _>(&connection, rows::user())
            .await
            .expect("the creator is written");
        crate::rows::insert::<EUser, _>(&connection, member.clone())
            .await
            .expect("the member is written");
        crate::rows::insert::<EProject, _>(&connection, rows::project())
            .await
            .expect("the project is written");
        crate::rows::insert::<gradient_entity::role::Entity, _>(&connection, role.clone())
            .await
            .expect("the role is written");
        crate::rows::insert::<gradient_entity::project_user::Entity, _>(
            &connection,
            gradient_entity::project_user::Model {
                user: member.id,
                ..rows::project_member(role.id)
            },
        )
        .await
        .expect("the membership is written");

        EProject::delete_by_id(project_id())
            .exec(&connection)
            .await
            .expect("the project with members deletes");

        assert!(
            db.find::<gradient_entity::role::Entity>(role.id)
                .await
                .is_none()
        );
        assert!(
            db.rows::<gradient_entity::project_user::Entity>()
                .await
                .is_empty()
        );
        db.finish().await;
    }
}
