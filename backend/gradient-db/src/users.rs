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
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbErr,
    EntityTrait, IntoActiveModel, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Select,
    TransactionTrait,
};
use std::future::Future;
use std::pin::Pin;

pub async fn by_username<C: ConnectionTrait>(
    db: &C,
    username: &str,
) -> Result<Option<MUser>, DbErr> {
    EUser::find()
        .filter(username_equals_ignoring_case_sql_filter(username))
        .one(db)
        .await
}

pub async fn find_user_for_password_login<C: ConnectionTrait>(
    db: &C,
    login_name: &str,
) -> Result<Option<MUser>, DbErr> {
    if login_name.contains('@') {
        by_email(db, login_name).await
    } else {
        by_username(db, login_name).await
    }
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

pub enum ScimUserFilter {
    Any,
    Username(String),
    ScimExternalId(String),
}

fn build_scim_provisioned_users_select_query_for_filter(filter: &ScimUserFilter) -> Select<EUser> {
    let query = EUser::find()
        .filter(CUser::AccountOrigin.eq(AccountOrigin::ScimProvisionedByIdentityProvider));
    match filter {
        ScimUserFilter::Any => query,
        ScimUserFilter::Username(username) => {
            query.filter(username_equals_ignoring_case_sql_filter(username))
        }
        ScimUserFilter::ScimExternalId(external_id) => {
            query.filter(CUser::ScimExternalId.eq(external_id.as_str()))
        }
    }
}

pub async fn count_scim_provisioned_users_matching_filter<C: ConnectionTrait>(
    db: &C,
    filter: &ScimUserFilter,
) -> Result<u64, DbErr> {
    build_scim_provisioned_users_select_query_for_filter(filter)
        .count(db)
        .await
}

pub async fn list_scim_provisioned_users_page_matching_filter<C: ConnectionTrait>(
    db: &C,
    filter: &ScimUserFilter,
    offset: u64,
    limit: u64,
) -> Result<Vec<MUser>, DbErr> {
    build_scim_provisioned_users_select_query_for_filter(filter)
        .order_by_asc(CUser::Id)
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
}

pub async fn fetch_scim_provisioned_user_by_id<C: ConnectionTrait>(
    db: &C,
    id: UserId,
) -> Result<Option<MUser>, DbErr> {
    build_scim_provisioned_users_select_query_for_filter(&ScimUserFilter::Any)
        .filter(CUser::Id.eq(id))
        .one(db)
        .await
}

pub struct ValidatedNewUser {
    id: UserId,
    username: String,
    display_name: String,
    email: String,
    origin: AccountOrigin,
    password_hash: Option<String>,
    email_verified: bool,
    pending_email_verification: Option<(String, NaiveDateTime)>,
    superuser: bool,
    active: bool,
    scim_external_id: Option<String>,
}

impl ValidatedNewUser {
    pub fn new(
        origin: AccountOrigin,
        username: &str,
        display_name: &str,
        email: Option<&str>,
    ) -> Result<Self, input::InputError> {
        match origin {
            AccountOrigin::DeclaredInStateFile => input::validate_username_syntax(username)?,
            _ => input::validate_username(username)?,
        }
        input::validate_display_name(display_name)?;
        let email = email.filter(|email| !email.is_empty());
        if let Some(email) = email {
            input::validate_email(email)?;
        }
        Ok(Self {
            id: UserId::now_v7(),
            username: username.to_owned(),
            display_name: display_name.to_owned(),
            email: email.unwrap_or_default().to_owned(),
            origin,
            password_hash: None,
            email_verified: false,
            pending_email_verification: None,
            superuser: false,
            active: true,
            scim_external_id: None,
        })
    }

    pub fn with_password_hash(mut self, password_hash: Option<String>) -> Self {
        self.password_hash = password_hash;
        self
    }

    pub fn with_email_verified(mut self, email_verified: bool) -> Self {
        self.email_verified = email_verified;
        self
    }

    pub fn with_pending_email_verification_token_and_expiry(
        mut self,
        token: String,
        expires_at: NaiveDateTime,
    ) -> Self {
        self.email_verified = false;
        self.pending_email_verification = Some((token, expires_at));
        self
    }

    pub fn with_superuser_flag(mut self, superuser: bool) -> Self {
        self.superuser = superuser;
        self
    }

    pub fn with_scim_external_id(mut self, external_id: Option<String>) -> Self {
        self.scim_external_id = external_id;
        self
    }

    pub fn with_active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn with_database_id(mut self, id: UserId) -> Self {
        self.id = id;
        self
    }
}

pub async fn insert_user_account<C: ConnectionTrait>(
    db: &C,
    new_user: ValidatedNewUser,
) -> Result<MUser, DbErr> {
    let now = gradient_types::now();
    let last_login_at = match new_user.origin {
        AccountOrigin::CreatedOnFirstSignInWithExternalLoginProvider => now,
        _ => *NULL_TIME,
    };
    let (email_verification_token, email_verification_token_expires) =
        new_user.pending_email_verification.unzip();
    MUser {
        id: new_user.id,
        username: new_user.username,
        name: new_user.display_name,
        email: new_user.email,
        password: new_user.password_hash,
        last_login_at,
        created_at: now,
        email_verified: new_user.email_verified,
        email_verification_token,
        email_verification_token_expires,
        managed: matches!(
            new_user.origin,
            AccountOrigin::DeclaredInStateFile | AccountOrigin::ScimProvisionedByIdentityProvider
        ),
        superuser: new_user.superuser,
        active: new_user.active,
        scim_external_id: new_user.scim_external_id,
        last_seen_version: None,
        last_opened_yearly_recap_year: None,
        account_origin: new_user.origin,
    }
    .into_active_model()
    .insert(db)
    .await
}

#[derive(Debug, Clone)]
pub enum VerifiedFactForSessionOrCredentialCreation {
    ExistingSignInSession(SessionId),
    PasswordHashStillEqual(String),
    ExternalLoginLink { issuer: String, subject: String },
}

impl VerifiedFactForSessionOrCredentialCreation {
    async fn still_holds(&self, tx: &DatabaseTransaction, user: &MUser) -> Result<bool, DbErr> {
        if !user.active {
            return Ok(false);
        }
        Ok(match self {
            Self::ExistingSignInSession(session) => ESession::find_by_id(*session)
                .filter(CSession::UserId.eq(user.id))
                .one(tx)
                .await?
                .is_some_and(|session| session.is_usable_at(gradient_types::now())),
            Self::PasswordHashStillEqual(hash) => user.password.as_ref() == Some(hash),
            Self::ExternalLoginLink { issuer, subject } => EUserIdentity::find()
                .filter(CUserIdentity::Issuer.eq(issuer.as_str()))
                .filter(CUserIdentity::Subject.eq(subject.as_str()))
                .filter(CUserIdentity::User.eq(user.id))
                .one(tx)
                .await?
                .is_some(),
        })
    }
}

#[derive(Debug)]
pub enum InsertWithAuthorisingSessionError<E> {
    AuthorisationNoLongerHoldsUnderUserRowLock,
    Database(DbErr),
    InsertClosureError(E),
}

impl<E> From<DbErr> for InsertWithAuthorisingSessionError<E> {
    fn from(error: DbErr) -> Self {
        Self::Database(error)
    }
}

pub async fn insert_credential_after_locking_user_row_and_rechecking_authorisation<T, E, F>(
    db: &DatabaseConnection,
    user: UserId,
    fact: VerifiedFactForSessionOrCredentialCreation,
    insert: F,
) -> Result<T, InsertWithAuthorisingSessionError<E>>
where
    F: for<'tx> FnOnce(
        &'tx DatabaseTransaction,
    ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'tx>>,
{
    let tx = db.begin().await?;
    let still_holds = match lock_user_row_for_update(&tx, user).await? {
        Some(user_row) => fact.still_holds(&tx, &user_row).await?,
        None => false,
    };
    if !still_holds {
        tx.rollback().await?;
        return Err(InsertWithAuthorisingSessionError::AuthorisationNoLongerHoldsUnderUserRowLock);
    }
    match insert(&tx).await {
        Ok(inserted) => {
            tx.commit().await?;
            Ok(inserted)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(InsertWithAuthorisingSessionError::InsertClosureError(error))
        }
    }
}

pub async fn lock_user_row_for_update<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<Option<MUser>, DbErr> {
    EUser::find_by_id(user).lock_exclusive().one(db).await
}

pub async fn revoke_previous_account_holder_access<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<(), DbErr> {
    lock_user_row_for_update(db, user).await?;
    ETeamUser::delete_many()
        .filter(CTeamUser::User.eq(user))
        .filter(CTeamUser::Source.eq(TeamMemberSource::SignInGroupSync))
        .exec(db)
        .await?;
    EUser::update_many()
        .col_expr(CUser::Password, Expr::value(Option::<String>::None))
        .filter(CUser::Id.eq(user))
        .exec(db)
        .await?;
    ESession::delete_many()
        .filter(CSession::UserId.eq(user))
        .exec(db)
        .await?;
    EApi::delete_many()
        .filter(CApi::OwnedBy.eq(user))
        .filter(CApi::Managed.eq(false))
        .exec(db)
        .await?;
    EUserSshKey::delete_many()
        .filter(CUserSshKey::User.eq(user))
        .exec(db)
        .await?;
    Ok(())
}

pub async fn delete_account_identity_links_and_clear_scim_external_id<C: ConnectionTrait>(
    db: &C,
    user: UserId,
) -> Result<(), DbErr> {
    EUserIdentity::delete_many()
        .filter(CUserIdentity::User.eq(user))
        .exec(db)
        .await?;
    ETeamUser::delete_many()
        .filter(CTeamUser::User.eq(user))
        .filter(CTeamUser::Source.eq(TeamMemberSource::ScimGroup))
        .exec(db)
        .await?;
    EUser::update_many()
        .col_expr(CUser::ScimExternalId, Expr::value(Option::<String>::None))
        .filter(CUser::Id.eq(user))
        .exec(db)
        .await?;
    Ok(())
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

    fn build_user_insert_sql_with_old_schema_columns(users: &[MUser]) -> String {
        let rows = users
            .iter()
            .map(|u| {
                format!(
                    "('{}', '{}', '{}', '{}', '{}', '{}', {}, {}, {})",
                    u.id,
                    u.username,
                    u.name,
                    u.email,
                    u.last_login_at,
                    u.created_at,
                    u.email_verified,
                    u.managed,
                    u.active
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "INSERT INTO \"user\" (id, username, name, email, last_login_at, created_at, \
             email_verified, managed, active) VALUES {rows}"
        )
    }

    #[tokio::test]
    async fn test_real_db_user_lookups_by_email_ignoring_case_password_login_routing_by_at_sign_and_username_search_escaping_wildcards_and_ordering()
     {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        for (username, email) in [
            ("alice", "Alice@Example.com"),
            ("bob", "bob@example.com"),
            ("x@y", "other@example.com"),
            ("robert", "bob"),
            ("zz-A_B%", "z@example.com"),
            ("a_b%first", "f@example.com"),
            ("a_bx", "g@example.com"),
            ("axb%", "h@example.com"),
        ] {
            let account = MUser {
                id: UserId::now_v7(),
                username: username.into(),
                name: username.into(),
                email: email.into(),
                active: true,
                ..Default::default()
            };
            crate::rows::insert::<EUser, _>(&conn, account)
                .await
                .expect("the account is written");
        }
        let username_of = |user: Option<MUser>| user.map(|u| u.username);

        let by_mixed_case = by_email_ignoring_case(&conn, "alice@EXAMPLE.com").await;
        let email_login = find_user_for_password_login(&conn, "x@y").await;
        let email_login_of_other_account =
            find_user_for_password_login(&conn, "other@example.com").await;
        let username_login = find_user_for_password_login(&conn, "bob").await;
        let found = search_by_username(&conn, "a_b%", 10).await.unwrap();
        let limited = search_by_username(&conn, "a_b%", 1).await.unwrap();

        assert_eq!(
            username_of(by_mixed_case.unwrap()).as_deref(),
            Some("alice")
        );
        assert_eq!(
            username_of(email_login.unwrap()),
            None,
            "a name with an at sign is looked up as an email only"
        );
        assert_eq!(
            username_of(email_login_of_other_account.unwrap()).as_deref(),
            Some("x@y")
        );
        assert_eq!(
            username_of(username_login.unwrap()).as_deref(),
            Some("bob"),
            "a name without an at sign is looked up as a username only"
        );
        assert_eq!(
            found
                .iter()
                .map(|u| u.username.as_str())
                .collect::<Vec<_>>(),
            ["a_b%first", "zz-A_B%"]
        );
        assert_eq!(limited.len(), 1);
        db.finish().await;
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
        db.execute(&build_user_insert_sql_with_old_schema_columns(&accounts))
            .await;

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
    async fn account_origin_migration_backfills_every_kind_of_existing_account() {
        use gradient_migration::{Migrator, MigratorTrait};
        use sea_orm_migration::SchemaManager;

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let conn = db.connection();
        let cases = [
            (
                "scim",
                true,
                Some("idp-1"),
                None,
                false,
                AccountOrigin::ScimProvisionedByIdentityProvider,
            ),
            (
                "state-on-scim",
                true,
                Some("idp-2"),
                Some("hash"),
                false,
                AccountOrigin::DeclaredInStateFile,
            ),
            (
                "state",
                true,
                None,
                Some("hash"),
                false,
                AccountOrigin::DeclaredInStateFile,
            ),
            (
                "external",
                false,
                None,
                None,
                true,
                AccountOrigin::CreatedOnFirstSignInWithExternalLoginProvider,
            ),
            (
                "linked-password",
                false,
                None,
                Some("hash"),
                true,
                AccountOrigin::PublicRegistration,
            ),
            (
                "registered",
                false,
                None,
                None,
                false,
                AccountOrigin::PublicRegistration,
            ),
        ];
        let mut expected = Vec::new();
        for (username, managed, scim_external_id, password, linked, origin) in cases {
            let user = crate::rows::insert::<EUser, _>(
                &conn,
                MUser {
                    id: UserId::now_v7(),
                    username: username.into(),
                    name: username.into(),
                    email: format!("{username}@x.io"),
                    managed,
                    scim_external_id: scim_external_id.map(str::to_owned),
                    password: password.map(str::to_owned),
                    active: true,
                    ..Default::default()
                },
            )
            .await
            .expect("the account is written");
            if linked {
                crate::user_identities::insert(&conn, user.id, "https://idp.example.com", username)
                    .await
                    .expect("the identity link is written");
            }
            expected.push((
                user.id,
                user.scim_external_id
                    .filter(|_| origin != AccountOrigin::DeclaredInStateFile),
                origin,
            ));
        }
        let migration = Migrator::migrations()
            .into_iter()
            .find(|m| {
                m.name()
                    == "m20261222_000000_add_user_origin_column_and_backfill_from_scim_external_id_managed_flag_and_linked_external_identities"
            })
            .expect("the migration is registered");

        migration
            .down(&SchemaManager::new(&conn))
            .await
            .expect("down runs");
        migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("up runs");

        for (id, scim_external_id, origin) in expected {
            let stored = db.find::<EUser>(id).await.expect("the account exists");
            assert_eq!(stored.account_origin, origin, "{}", stored.username);
            assert_eq!(
                stored.scim_external_id, scim_external_id,
                "{}",
                stored.username
            );
        }
        db.finish().await;
    }

    #[tokio::test]
    async fn old_server_inserted_user_with_scim_external_id_has_scim_provisioned_account_origin_and_without_has_public_registration()
     {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let (scim, registered) = (UserId::now_v7(), UserId::now_v7());
        db.execute(&format!(
            "INSERT INTO \"user\" (id, username, name, email, last_login_at, created_at, \
             managed, active, scim_external_id) VALUES \
             ('{scim}', 'scim', 'Scim', 'scim@x.io', now(), now(), true, true, 'idp-1'), \
             ('{registered}', 'registered', 'Registered', 'registered@x.io', now(), now(), \
             false, true, NULL);"
        ))
        .await;

        for (id, origin) in [
            (scim, AccountOrigin::ScimProvisionedByIdentityProvider),
            (registered, AccountOrigin::PublicRegistration),
        ] {
            let stored = db.find::<EUser>(id).await.expect("the account exists");
            assert_eq!(stored.account_origin, origin, "{}", stored.username);
        }
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
        let found_for_login = find_user_for_password_login(&db.connection(), "ALICE@example.com")
            .await
            .expect("the lookup runs")
            .map(|user| user.username);
        let second = crate::rows::insert::<EUser, _>(
            &db.connection(),
            account("mallory", "Alice@example.COM"),
        )
        .await;

        assert_eq!(found.as_deref(), Some("alice"), "the lookup ignores case");
        assert_eq!(found_for_login.as_deref(), Some("alice"));
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
        db.execute(&build_user_insert_sql_with_old_schema_columns(&accounts))
            .await;

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

    async fn create_account_with_live_and_deleted_session(
        db: &gradient_fixtures::database::TestDatabase,
    ) -> (UserId, SessionId, SessionId) {
        use gradient_fixtures::rows;

        let connection = db.connection();
        let owner = crate::rows::insert::<EUser, _>(&connection, rows::user())
            .await
            .expect("the account is written");
        let live = crate::rows::insert::<ESession, _>(
            &connection,
            rows::live_session(SessionId::now_v7()),
        )
        .await
        .expect("the live session is written");
        let deleted = crate::rows::insert::<ESession, _>(
            &connection,
            rows::live_session(SessionId::now_v7()),
        )
        .await
        .expect("the session to delete is written");
        let deleted_id = deleted.id;
        crate::rows::delete::<ESession, _>(&connection, deleted)
            .await
            .expect("the session is deleted");
        (owner.id, live.id, deleted_id)
    }

    #[tokio::test]
    async fn test_create_api_key_refuses_deleted_authorising_session_and_writes_with_live_session()
    {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        let (owner, live, deleted) = create_account_with_live_and_deleted_session(&db).await;
        let api_key = |name: &str| MApi {
            id: ApiId::now_v7(),
            owned_by: owner,
            name: name.into(),
            key: format!("hash-{name}"),
            ..Default::default()
        };

        let refused_key = api_key("refused");
        let refused = insert_credential_after_locking_user_row_and_rechecking_authorisation(
            &connection,
            owner,
            VerifiedFactForSessionOrCredentialCreation::ExistingSignInSession(deleted),
            |tx| Box::pin(async move { crate::rows::insert::<EApi, _>(tx, refused_key).await }),
        )
        .await;
        let written_key = api_key("written");
        let written = insert_credential_after_locking_user_row_and_rechecking_authorisation(
            &connection,
            owner,
            VerifiedFactForSessionOrCredentialCreation::ExistingSignInSession(live),
            |tx| Box::pin(async move { crate::rows::insert::<EApi, _>(tx, written_key).await }),
        )
        .await;

        assert!(matches!(
            refused,
            Err(InsertWithAuthorisingSessionError::AuthorisationNoLongerHoldsUnderUserRowLock)
        ));
        assert_eq!(written.expect("the key is written").name, "written");
        let names: Vec<String> = db
            .rows::<EApi>()
            .await
            .into_iter()
            .map(|k| k.name)
            .collect();
        assert_eq!(names, ["written"]);
        db.finish().await;
    }

    #[tokio::test]
    async fn add_ssh_key_with_deleted_authorising_session_is_refused_and_writes_no_ssh_key_row() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        let (owner, _, deleted) = create_account_with_live_and_deleted_session(&db).await;
        let ssh_key = MUserSshKey {
            id: UserSshKeyId::now_v7(),
            user: owner,
            name: "laptop".into(),
            public_key: "ssh-ed25519 AAAA".into(),
            fingerprint: "SHA256:laptop".into(),
            ..Default::default()
        };

        let refused = insert_credential_after_locking_user_row_and_rechecking_authorisation(
            &connection,
            owner,
            VerifiedFactForSessionOrCredentialCreation::ExistingSignInSession(deleted),
            |tx| Box::pin(async move { crate::rows::insert::<EUserSshKey, _>(tx, ssh_key).await }),
        )
        .await;

        assert!(matches!(
            refused,
            Err(InsertWithAuthorisingSessionError::AuthorisationNoLongerHoldsUnderUserRowLock)
        ));
        assert!(db.rows::<EUserSshKey>().await.is_empty());
        db.finish().await;
    }

    async fn insert_session_authorised_by(
        connection: &DatabaseConnection,
        owner: UserId,
        fact: VerifiedFactForSessionOrCredentialCreation,
    ) -> Result<MSession, InsertWithAuthorisingSessionError<DbErr>> {
        let session = gradient_fixtures::rows::live_session(SessionId::now_v7());
        insert_credential_after_locking_user_row_and_rechecking_authorisation(
            connection,
            owner,
            fact,
            |tx| Box::pin(async move { crate::rows::insert::<ESession, _>(tx, session).await }),
        )
        .await
    }

    #[tokio::test]
    async fn session_insert_with_verified_password_hash_writes_only_when_stored_hash_matches() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        let owner = crate::rows::insert::<EUser, _>(&connection, gradient_fixtures::rows::user())
            .await
            .expect("the account is written");
        let verified_hash = owner.password.clone().expect("the account has a password");

        let written = insert_session_authorised_by(
            &connection,
            owner.id,
            VerifiedFactForSessionOrCredentialCreation::PasswordHashStillEqual(
                verified_hash.clone(),
            ),
        )
        .await;
        EUser::update_many()
            .col_expr(CUser::Password, Expr::value("hash-of-the-new-holder"))
            .filter(CUser::Id.eq(owner.id))
            .exec(&connection)
            .await
            .expect("the password is replaced");
        let refused = insert_session_authorised_by(
            &connection,
            owner.id,
            VerifiedFactForSessionOrCredentialCreation::PasswordHashStillEqual(verified_hash),
        )
        .await;

        let written = written.expect("the session is written");
        assert!(matches!(
            refused,
            Err(InsertWithAuthorisingSessionError::AuthorisationNoLongerHoldsUnderUserRowLock)
        ));
        let sessions: Vec<SessionId> = db.rows::<ESession>().await.iter().map(|s| s.id).collect();
        assert_eq!(sessions, [written.id]);
        db.finish().await;
    }

    #[tokio::test]
    async fn session_insert_is_refused_when_identity_link_is_deleted() {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        let owner = crate::rows::insert::<EUser, _>(&connection, gradient_fixtures::rows::user())
            .await
            .expect("the account is written");
        crate::user_identities::insert(&connection, owner.id, "https://idp", "subject-1")
            .await
            .expect("the identity link is written");
        let link = || VerifiedFactForSessionOrCredentialCreation::ExternalLoginLink {
            issuer: "https://idp".into(),
            subject: "subject-1".into(),
        };

        let written = insert_session_authorised_by(&connection, owner.id, link()).await;
        delete_account_identity_links_and_clear_scim_external_id(&connection, owner.id)
            .await
            .expect("the identity link is deleted");
        let refused = insert_session_authorised_by(&connection, owner.id, link()).await;

        let written = written.expect("the session is written");
        assert!(matches!(
            refused,
            Err(InsertWithAuthorisingSessionError::AuthorisationNoLongerHoldsUnderUserRowLock)
        ));
        let sessions: Vec<SessionId> = db.rows::<ESession>().await.iter().map(|s| s.id).collect();
        assert_eq!(sessions, [written.id]);
        db.finish().await;
    }

    #[tokio::test]
    async fn credential_insert_with_expired_or_revoked_session_is_refused() {
        use gradient_fixtures::rows;

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        let owner = crate::rows::insert::<EUser, _>(&connection, rows::user())
            .await
            .expect("the account is written");
        let now = gradient_types::now();
        let expired = MSession {
            expires_at: now - chrono::Duration::minutes(1),
            ..rows::live_session(SessionId::now_v7())
        };
        let revoked = MSession {
            revoked_at: Some(now),
            ..rows::live_session(SessionId::now_v7())
        };
        let live = rows::live_session(SessionId::now_v7());
        for session in [expired.clone(), revoked.clone(), live.clone()] {
            crate::rows::insert::<ESession, _>(&connection, session)
                .await
                .expect("the session is written");
        }
        let insert_key = |session: SessionId, name: &str| {
            let key = MApi {
                id: ApiId::now_v7(),
                owned_by: owner.id,
                name: name.into(),
                key: format!("hash-{name}"),
                ..Default::default()
            };
            insert_credential_after_locking_user_row_and_rechecking_authorisation(
                &connection,
                owner.id,
                VerifiedFactForSessionOrCredentialCreation::ExistingSignInSession(session),
                |tx| Box::pin(async move { crate::rows::insert::<EApi, _>(tx, key).await }),
            )
        };

        let from_expired = insert_key(expired.id, "expired").await;
        let from_revoked = insert_key(revoked.id, "revoked").await;
        let from_live = insert_key(live.id, "live").await;

        for refused in [from_expired, from_revoked] {
            assert!(matches!(
                refused,
                Err(InsertWithAuthorisingSessionError::AuthorisationNoLongerHoldsUnderUserRowLock)
            ));
        }
        assert_eq!(from_live.expect("the key is written").name, "live");
        let names: Vec<String> = db
            .rows::<EApi>()
            .await
            .into_iter()
            .map(|k| k.name)
            .collect();
        assert_eq!(names, ["live"]);
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
