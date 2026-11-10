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
    PaginatorTrait, QueryFilter, QuerySelect, Select,
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
                .add(CUser::Email.eq(email)),
        )
        .one(db)
        .await
}

fn username_equals_ignoring_case_sql_filter(username: &str) -> SimpleExpr {
    Expr::expr(Func::lower(Expr::col((EUser::default(), CUser::Username))))
        .eq(username.to_lowercase())
}

pub async fn by_email<C: ConnectionTrait>(db: &C, email: &str) -> Result<Option<MUser>, DbErr> {
    EUser::find().filter(CUser::Email.eq(email)).one(db).await
}

pub async fn by_email_ignoring_case<C: ConnectionTrait>(
    db: &C,
    email: &str,
) -> Result<Option<MUser>, DbErr> {
    EUser::find()
        .filter(
            Expr::expr(Func::lower(Expr::col((EUser::default(), CUser::Email))))
                .eq(email.to_lowercase()),
        )
        .one(db)
        .await
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
}
