/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::StateConfiguration;
use crate::provisioning::{DynError, apply_state_to_database};
use gradient_db::users::{ValidatedNewUser, insert_user_account};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows;
use gradient_types::*;
use sea_orm::{ActiveModelTrait, DatabaseConnection, IntoActiveModel};
use ssh_key::private::Ed25519Keypair;
use ssh_key::{LineEnding, PrivateKey};
use std::fs;
use tempfile::TempDir;

const PASSWORD_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$dGVzdA";
const OTHER_PASSWORD_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$b3RoZXJzYWx0$b3RoZXI";

struct TemporaryCredentialsDirectoryAndCryptSecretWithStateApply {
    credentials: TempDir,
}

impl TemporaryCredentialsDirectoryAndCryptSecretWithStateApply {
    fn new() -> Self {
        let credentials = TempDir::new().unwrap();
        fs::write(
            credentials.path().join("crypt_secret"),
            "0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        Self { credentials }
    }

    fn write(&self, kind: &str, name: &str, suffix: &str, contents: &str) {
        fs::write(
            self.credentials
                .path()
                .join(format!("gradient_{kind}_{name}_{suffix}")),
            contents,
        )
        .unwrap();
    }

    fn write_project_private_key(&self, project: &str) {
        let key = PrivateKey::from(Ed25519Keypair::from_seed(&[7; 32]));
        self.write(
            "project",
            project,
            "private_key",
            &key.to_openssh(LineEnding::LF).unwrap(),
        );
    }

    async fn apply(
        &self,
        db: &DatabaseConnection,
        state_json: &str,
        delete_state: bool,
    ) -> Result<(), DynError> {
        let config: StateConfiguration = serde_json::from_str(state_json).unwrap();
        let crypt_secret_file = self.credentials.path().join("crypt_secret");
        apply_state_to_database(
            db,
            &config,
            crypt_secret_file.to_str().unwrap(),
            self.credentials.path(),
            delete_state,
            false,
            &gradient_util::http_validation::UserUrlDestinationAllowlist::default(),
        )
        .await
    }
}

async fn insert_publicly_registered_account_with_password_hash(
    db: &DatabaseConnection,
    username: &str,
    email: &str,
) -> MUser {
    let new_user = ValidatedNewUser::new(
        AccountOrigin::PublicRegistration,
        username,
        username,
        Some(email),
    )
    .unwrap()
    .with_password_hash(Some(OTHER_PASSWORD_HASH.to_owned()))
    .with_email_verified(true);
    insert_user_account(db, new_user).await.unwrap()
}

async fn reload(test_db: &TestDatabase, user: &MUser) -> MUser {
    test_db.find::<EUser>(user.id).await.unwrap()
}

#[tokio::test]
async fn apply_does_not_take_over_self_registered_account_from_other_origin_by_username() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let bob =
        insert_publicly_registered_account_with_password_hash(&db, "bob", "bob@self.io").await;
    rows::admin_role()
        .into_active_model()
        .insert(&db)
        .await
        .unwrap();
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();
    state.write_project_private_key("acme");

    state
        .apply(
            &db,
            r#"{
                "users": {
                    "admin": { "username": "admin", "name": "Admin", "email": "admin@x.io" },
                    "bob": { "username": "bob", "name": "Bob", "email": "bob@state.io" }
                },
                "projects": {
                    "acme": {
                        "name": "acme", "display_name": "ACME", "private_key_file": "k",
                        "public": false, "created_by": "admin",
                        "members": [ { "user": "admin", "role": "Admin" }, { "user": "bob", "role": "Admin" } ]
                    }
                }
            }"#,
            true,
        )
        .await
        .unwrap();

    let after = reload(&test_db, &bob).await;
    assert_eq!(after.account_origin, AccountOrigin::PublicRegistration);
    assert_eq!(after.password.as_deref(), Some(OTHER_PASSWORD_HASH));
    assert_eq!(after.email, "bob@self.io");
    assert!(!after.managed);
    let memberships = test_db.rows::<EProjectUser>().await;
    assert!(
        memberships
            .iter()
            .all(|membership| membership.user != bob.id)
    );
    assert_eq!(memberships.len(), 1);
    test_db.finish().await;
}

#[tokio::test]
async fn test_case_only_username_clash_does_not_wipe_other_account_password() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let alice =
        insert_publicly_registered_account_with_password_hash(&db, "Alice", "alice@self.io").await;
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();

    state
        .apply(
            &db,
            r#"{ "users": { "alice": {
                "username": "alice", "name": "State Alice", "email": "alice@state.io", "superuser": true
            } } }"#,
            true,
        )
        .await
        .unwrap();

    let after = reload(&test_db, &alice).await;
    assert_eq!(after, alice);
    assert_eq!(test_db.rows::<EUser>().await.len(), 1);
    test_db.finish().await;
}

#[tokio::test]
async fn test_scim_provisioned_undeclared_account_survives_state_apply_with_delete_enabled() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let scim_user = insert_user_account(
        &db,
        ValidatedNewUser::new(
            AccountOrigin::ScimProvisionedByIdentityProvider,
            "carol",
            "Carol",
            Some("carol@idp.io"),
        )
        .unwrap()
        .with_scim_external_id(Some("idp-carol".to_owned())),
    )
    .await
    .unwrap();
    assert!(scim_user.managed);
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();

    state.apply(&db, r#"{ "users": {} }"#, true).await.unwrap();

    assert_eq!(reload(&test_db, &scim_user).await, scim_user);
    test_db.finish().await;
}

#[tokio::test]
async fn test_apply_removed_state_superuser_with_api_key_with_delete_disabled_deactivates_account_and_deletes_api_key()
 {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();
    state.write("user", "root", "password", PASSWORD_HASH);
    state.write("api", "ci", "key", &"ab".repeat(32));

    state
        .apply(
            &db,
            r#"{
                "users": { "root": {
                    "username": "root", "name": "Root", "email": "root@x.io",
                    "password_file": "p", "superuser": true
                } },
                "api_keys": { "ci": { "name": "ci", "key_file": "k", "owned_by": "root", "cache_permissions": ["readStore"] } }
            }"#,
            false,
        )
        .await
        .unwrap();
    assert_eq!(test_db.rows::<EApi>().await.len(), 1);

    state.apply(&db, r#"{ "users": {} }"#, false).await.unwrap();

    let users = test_db.rows::<EUser>().await;
    assert_eq!(users.len(), 1);
    assert!(!users[0].superuser);
    assert!(!users[0].active);
    assert!(!users[0].managed);
    assert!(test_db.rows::<EApi>().await.is_empty());
    test_db.finish().await;
}

#[tokio::test]
async fn test_apply_that_fails_later_leaves_no_user_row() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();

    let result = state
        .apply(
            &db,
            r#"{
                "users": { "admin": { "username": "admin", "name": "Admin", "email": "admin@x.io" } },
                "projects": { "acme": {
                    "name": "acme", "display_name": "ACME", "private_key_file": "missing",
                    "public": false, "created_by": "admin"
                } }
            }"#,
            true,
        )
        .await;

    assert!(result.is_err());
    assert!(test_db.rows::<EUser>().await.is_empty());
    test_db.finish().await;
}

#[tokio::test]
async fn test_project_created_by_skipped_user_causes_state_apply_error() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let bob =
        insert_publicly_registered_account_with_password_hash(&db, "bob", "bob@self.io").await;
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();
    state.write_project_private_key("acme");

    let result = state
        .apply(
            &db,
            r#"{
                "users": { "bob": { "username": "bob", "name": "Bob", "email": "bob@state.io" } },
                "projects": { "acme": {
                    "name": "acme", "display_name": "ACME", "private_key_file": "k",
                    "public": false, "created_by": "bob"
                } }
            }"#,
            true,
        )
        .await;

    assert!(result.is_err());
    assert_eq!(reload(&test_db, &bob).await, bob);
    assert!(test_db.rows::<EProject>().await.is_empty());
    test_db.finish().await;
}

#[tokio::test]
async fn test_declared_user_with_existing_account_id_takes_over_self_registered_account() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let bob =
        insert_publicly_registered_account_with_password_hash(&db, "bob", "bob@self.io").await;
    assert!(bob.password.is_some());
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();
    let state_json = format!(
        r#"{{ "users": {{ "robert": {{
            "username": "robert", "name": "Robert", "email": "robert@state.io",
            "existing_account_id": "{}"
        }} }} }}"#,
        bob.id
    );

    state.apply(&db, &state_json, true).await.unwrap();

    let after = reload(&test_db, &bob).await;
    assert_eq!(after.account_origin, AccountOrigin::DeclaredInStateFile);
    assert!(after.managed);
    assert_eq!(after.username, "robert");
    assert_eq!(after.email, "robert@state.io");
    assert_eq!(after.password, None);
    assert_eq!(test_db.rows::<EUser>().await.len(), 1);
    test_db.finish().await;
}

#[tokio::test]
async fn test_existing_project_keeps_stored_creator_when_declared_creator_name_belongs_to_other_origin()
 {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let founder =
        insert_publicly_registered_account_with_password_hash(&db, "founder", "founder@self.io")
            .await;
    insert_publicly_registered_account_with_password_hash(&db, "ops", "ops@self.io").await;
    let project = MProject {
        id: ProjectId::now_v7(),
        name: "acme".into(),
        created_by: founder.id,
        ..rows::project()
    }
    .into_active_model()
    .insert(&db)
    .await
    .unwrap();
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();
    state.write_project_private_key("acme");

    state
        .apply(
            &db,
            r#"{
                "users": { "ops": { "username": "ops", "name": "Ops", "email": "ops@state.io" } },
                "projects": { "acme": {
                    "name": "acme", "display_name": "ACME", "private_key_file": "k",
                    "public": false, "created_by": "ops"
                } }
            }"#,
            true,
        )
        .await
        .unwrap();

    let after = test_db.find::<EProject>(project.id).await.unwrap();
    assert_eq!(after.created_by, founder.id);
    assert_eq!(after.display_name, "ACME");
    assert!(test_db.rows::<EProjectUser>().await.is_empty());
    test_db.finish().await;
}

#[tokio::test]
async fn test_export_and_apply_project_created_by_public_account_keeps_stored_creator() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let founder =
        insert_publicly_registered_account_with_password_hash(&db, "founder", "founder@self.io")
            .await;
    let project = MProject {
        id: ProjectId::now_v7(),
        name: "acme".into(),
        created_by: founder.id,
        ..rows::project()
    }
    .into_active_model()
    .insert(&db)
    .await
    .unwrap();
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();
    state.write_project_private_key("acme");
    let mut exported = crate::export::export_state(&db).await.unwrap();
    exported.projects.get_mut("acme").unwrap().private_key_file = "k".into();

    let validation = exported.validate();
    state
        .apply(&db, &serde_json::to_string(&exported).unwrap(), true)
        .await
        .unwrap();

    assert!(validation.is_valid, "{:?}", validation.errors);
    let after = test_db.find::<EProject>(project.id).await.unwrap();
    assert_eq!(after.created_by, founder.id);
    assert!(test_db.rows::<EProjectUser>().await.is_empty());
    test_db.finish().await;
}

#[tokio::test]
async fn test_declared_user_whose_email_alone_belongs_to_other_origin_is_skipped() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let other =
        insert_publicly_registered_account_with_password_hash(&db, "other", "Bob@State.io").await;
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();

    state
        .apply(
            &db,
            r#"{ "users": { "bob": {
                "username": "bob", "name": "Bob", "email": "bob@state.io", "superuser": true
            } } }"#,
            true,
        )
        .await
        .unwrap();

    assert_eq!(reload(&test_db, &other).await, other);
    assert_eq!(test_db.rows::<EUser>().await.len(), 1);
    test_db.finish().await;
}

#[tokio::test]
async fn test_export_then_apply_changes_no_account_of_any_origin() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let mut accounts = Vec::new();
    for (origin, username) in [
        (AccountOrigin::DeclaredInStateFile, "declared"),
        (AccountOrigin::PublicRegistration, "registered"),
        (
            AccountOrigin::ScimProvisionedByIdentityProvider,
            "provisioned",
        ),
        (
            AccountOrigin::CreatedOnFirstSignInWithExternalLoginProvider,
            "external",
        ),
    ] {
        let password_hash =
            (origin == AccountOrigin::PublicRegistration).then(|| OTHER_PASSWORD_HASH.to_owned());
        let new_user = ValidatedNewUser::new(
            origin,
            username,
            username,
            Some(&format!("{username}@x.io")),
        )
        .unwrap()
        .with_password_hash(password_hash);
        accounts.push(insert_user_account(&db, new_user).await.unwrap());
    }
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();
    let exported = serde_json::to_string(&crate::export::export_state(&db).await.unwrap()).unwrap();

    state.apply(&db, &exported, true).await.unwrap();
    state.apply(&db, &exported, true).await.unwrap();

    for account in &accounts {
        assert_eq!(&reload(&test_db, account).await, account);
    }
    test_db.finish().await;
}

#[tokio::test]
async fn test_export_state_users_contains_only_declared_account_not_public_registration_account() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    insert_publicly_registered_account_with_password_hash(&db, "registered", "registered@self.io")
        .await;
    let declared = ValidatedNewUser::new(
        AccountOrigin::DeclaredInStateFile,
        "declared",
        "declared",
        Some("declared@state.io"),
    )
    .unwrap();
    insert_user_account(&db, declared).await.unwrap();

    let exported = crate::export::export_state(&db).await.unwrap();

    let mut usernames: Vec<_> = exported.users.keys().cloned().collect();
    usernames.sort();
    assert_eq!(usernames, ["declared"]);
    test_db.finish().await;
}

async fn insert_session_api_key_identity_link_and_ssh_key_for_account(
    db: &DatabaseConnection,
    user: &MUser,
) {
    MSession {
        user_id: user.id,
        ..rows::live_session(SessionId::now_v7())
    }
    .into_active_model()
    .insert(db)
    .await
    .unwrap();
    MApi {
        id: ApiId::now_v7(),
        owned_by: user.id,
        name: "laptop".into(),
        key: "0".repeat(64),
        ..Default::default()
    }
    .into_active_model()
    .insert(db)
    .await
    .unwrap();
    gradient_db::user_identities::insert(db, user.id, "https://idp.example.com", "subject-1")
        .await
        .unwrap();
    MUserSshKey {
        id: UserSshKeyId::now_v7(),
        user: user.id,
        name: "laptop".into(),
        public_key: "ssh-ed25519 AAAA laptop".into(),
        fingerprint: "SHA256:laptop".into(),
        ..Default::default()
    }
    .into_active_model()
    .insert(db)
    .await
    .unwrap();
}

async fn take_over_by_existing_account_id_and_assert_previous_holder_lost_access(
    test_db: &TestDatabase,
    previous: &MUser,
) {
    let db = test_db.connection();
    insert_session_api_key_identity_link_and_ssh_key_for_account(&db, previous).await;
    assert!(reload(test_db, previous).await.password.is_some());
    assert_eq!(test_db.rows::<EUserSshKey>().await.len(), 1);
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();
    let state_json = format!(
        r#"{{ "users": {{ "robert": {{
            "username": "robert", "name": "Robert", "email": "robert@state.io",
            "existing_account_id": "{}"
        }} }} }}"#,
        previous.id
    );

    state.apply(&db, &state_json, true).await.unwrap();

    let after = reload(test_db, previous).await;
    assert_eq!(after.account_origin, AccountOrigin::DeclaredInStateFile);
    assert_eq!(after.password, None);
    assert_eq!(after.scim_external_id, None);
    assert!(test_db.rows::<ESession>().await.is_empty());
    assert!(test_db.rows::<EApi>().await.is_empty());
    assert!(test_db.rows::<EUserIdentity>().await.is_empty());
    assert!(test_db.rows::<EUserSshKey>().await.is_empty());
}

#[tokio::test]
async fn test_take_over_of_self_registered_account_by_id_removes_sessions_api_keys_and_identity_links()
 {
    let test_db = TestDatabase::create().await;
    let bob = insert_publicly_registered_account_with_password_hash(
        &test_db.connection(),
        "bob",
        "bob@self.io",
    )
    .await;

    take_over_by_existing_account_id_and_assert_previous_holder_lost_access(&test_db, &bob).await;
    test_db.finish().await;
}

#[tokio::test]
async fn test_take_over_of_scim_account_by_id_removes_scim_external_id_sessions_and_identity_links()
{
    let test_db = TestDatabase::create().await;
    let carol = insert_user_account(
        &test_db.connection(),
        ValidatedNewUser::new(
            AccountOrigin::ScimProvisionedByIdentityProvider,
            "carol",
            "Carol",
            Some("carol@idp.io"),
        )
        .unwrap()
        .with_scim_external_id(Some("idp-carol".to_owned()))
        .with_password_hash(Some(PASSWORD_HASH.to_owned())),
    )
    .await
    .unwrap();

    take_over_by_existing_account_id_and_assert_previous_holder_lost_access(&test_db, &carol).await;
    test_db.finish().await;
}

async fn insert_team_with_member(
    db: &DatabaseConnection,
    name: &str,
    user: &MUser,
    source: TeamMemberSource,
) -> TeamId {
    let team = MTeam {
        id: TeamId::now_v7(),
        name: name.into(),
        display_name: name.into(),
        created_by: None,
        ..rows::team()
    }
    .into_active_model()
    .insert(db)
    .await
    .unwrap();
    MTeamUser {
        team: team.id,
        user: user.id,
        source,
        ..rows::team_member(TeamRole::Member)
    }
    .into_active_model()
    .insert(db)
    .await
    .unwrap();
    team.id
}

#[tokio::test]
async fn test_take_over_of_scim_account_by_id_removes_sign_in_group_sync_and_scim_group_memberships_and_keeps_api_and_declared_memberships()
 {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let carol = insert_user_account(
        &db,
        ValidatedNewUser::new(
            AccountOrigin::ScimProvisionedByIdentityProvider,
            "carol",
            "Carol",
            Some("carol@idp.io"),
        )
        .unwrap()
        .with_scim_external_id(Some("idp-carol".to_owned())),
    )
    .await
    .unwrap();
    insert_team_with_member(&db, "a", &carol, TeamMemberSource::SignInGroupSync).await;
    let b = insert_team_with_member(&db, "b", &carol, TeamMemberSource::Api).await;
    insert_team_with_member(&db, "c", &carol, TeamMemberSource::ScimGroup).await;
    let e = insert_team_with_member(&db, "e", &carol, TeamMemberSource::SignInGroupSync).await;
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();
    let state_json = format!(
        r#"{{
            "users": {{ "robert": {{
                "username": "robert", "name": "Robert", "email": "robert@state.io",
                "existing_account_id": "{}"
            }} }},
            "teams": {{ "e": {{
                "name": "e", "display_name": "E",
                "members": [{{ "user": "robert", "role": "Member" }}]
            }} }}
        }}"#,
        carol.id
    );

    state.apply(&db, &state_json, true).await.unwrap();

    let mut memberships: Vec<(TeamId, TeamMemberSource)> = test_db
        .rows::<ETeamUser>()
        .await
        .into_iter()
        .map(|row| {
            assert_eq!(row.user, carol.id);
            (row.team, row.source)
        })
        .collect();
    memberships.sort_by_key(|(team, _)| *team);
    assert_eq!(
        memberships,
        vec![(b, TeamMemberSource::Api), (e, TeamMemberSource::State)]
    );
    test_db.finish().await;
}

#[tokio::test]
async fn test_insert_account_with_username_differing_only_by_letter_case_fails() {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    insert_publicly_registered_account_with_password_hash(&db, "Dave", "dave@x.io").await;

    let duplicate = ValidatedNewUser::new(
        AccountOrigin::PublicRegistration,
        "dave",
        "dave",
        Some("other@x.io"),
    )
    .unwrap();

    assert!(insert_user_account(&db, duplicate).await.is_err());
    assert_eq!(test_db.rows::<EUser>().await.len(), 1);
    test_db.finish().await;
}

#[tokio::test]
async fn test_take_over_of_self_registered_account_by_id_keeps_declared_api_key_id_and_deletes_previous_account_api_key()
 {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let bob =
        insert_publicly_registered_account_with_password_hash(&db, "bob", "bob@self.io").await;
    insert_session_api_key_identity_link_and_ssh_key_for_account(&db, &bob).await;
    let declared = MApi {
        id: ApiId::now_v7(),
        owned_by: bob.id,
        name: "ci".into(),
        key: "1".repeat(64),
        managed: true,
        ..Default::default()
    }
    .into_active_model()
    .insert(&db)
    .await
    .unwrap();
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();
    state.write("api", "ci", "key", &"ab".repeat(32));
    let state_json = format!(
        r#"{{
            "users": {{ "robert": {{
                "username": "robert", "name": "Robert", "email": "robert@state.io",
                "existing_account_id": "{}"
            }} }},
            "api_keys": {{ "ci": {{
                "name": "ci", "key_file": "k", "owned_by": "robert", "cache_permissions": ["readStore"]
            }} }}
        }}"#,
        bob.id
    );

    state.apply(&db, &state_json, true).await.unwrap();

    let keys = test_db.rows::<EApi>().await;
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].id, declared.id);
    assert!(keys[0].managed);
    assert_eq!(reload(&test_db, &bob).await.password, None);
    test_db.finish().await;
}

#[tokio::test]
async fn reapplying_state_file_to_owned_account_keeps_sessions_api_keys_login_links_ssh_keys_and_password()
 {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();
    state.write("user", "dora", "password", PASSWORD_HASH);
    let state_json = r#"{ "users": { "dora": {
        "username": "dora", "name": "Dora", "email": "dora@x.io", "password_file": "p"
    } } }"#;
    state.apply(&db, state_json, true).await.unwrap();
    let dora = test_db.rows::<EUser>().await.remove(0);
    assert!(dora.password.is_some());
    insert_session_api_key_identity_link_and_ssh_key_for_account(&db, &dora).await;

    state.apply(&db, state_json, true).await.unwrap();
    state.apply(&db, state_json, true).await.unwrap();

    assert_eq!(reload(&test_db, &dora).await, dora);
    assert_eq!(test_db.rows::<ESession>().await.len(), 1);
    assert_eq!(test_db.rows::<EApi>().await.len(), 1);
    assert_eq!(test_db.rows::<EUserIdentity>().await.len(), 1);
    assert_eq!(test_db.rows::<EUserSshKey>().await.len(), 1);
    test_db.finish().await;
}

#[tokio::test]
async fn test_apply_task_cache_and_git_host_connection_created_by_empty_keeps_stored_self_registered_creator()
 {
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let founder =
        insert_publicly_registered_account_with_password_hash(&db, "founder", "founder@self.io")
            .await;
    let project = MProject {
        id: ProjectId::now_v7(),
        name: "acme".into(),
        created_by: founder.id,
        ..rows::project()
    }
    .into_active_model()
    .insert(&db)
    .await
    .unwrap();
    let task = MTask {
        id: TaskId::now_v7(),
        project: project.id,
        name: "build".into(),
        created_by: founder.id,
        ..rows::task()
    }
    .into_active_model()
    .insert(&db)
    .await
    .unwrap();
    let cache = rows::cache_with_id(CacheId::now_v7(), "store", founder.id)
        .into_active_model()
        .insert(&db)
        .await
        .unwrap();
    let connection = MGitHostConnection {
        id: GitHostConnectionId::now_v7(),
        name: "forge".into(),
        display_name: "forge".into(),
        base_url: Some("https://git.example.com".into()),
        receives_events: true,
        created_by: founder.id,
        ..Default::default()
    }
    .into_active_model()
    .insert(&db)
    .await
    .unwrap();
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();
    state.write_project_private_key("acme");
    state.write("cache", "store", "signing_key", &"A".repeat(88));

    state
        .apply(
            &db,
            r#"{
                "projects": { "acme": {
                    "name": "acme", "display_name": "ACME", "private_key_file": "k",
                    "public": false, "created_by": ""
                } },
                "tasks": { "build": {
                    "name": "build", "project": "acme", "display_name": "Build",
                    "repository": "https://github.com/test/repo", "created_by": ""
                } },
                "caches": { "store": {
                    "name": "store", "display_name": "Store", "signing_key_file": "k",
                    "public": false, "created_by": ""
                } },
                "git_host_connections": { "forge": {
                    "name": "forge", "owner": "instance", "git_host_type": "gitea", "base_url": "https://git.example.com", "created_by": ""
                } }
            }"#,
            false,
        )
        .await
        .unwrap();

    let task_after = test_db.find::<ETask>(task.id).await.unwrap();
    let cache_after = test_db.find::<ECache>(cache.id).await.unwrap();
    let connection_after = test_db
        .find::<EGitHostConnection>(connection.id)
        .await
        .unwrap();
    assert_eq!(task_after.created_by, founder.id);
    assert_eq!(task_after.display_name, "Build");
    assert_eq!(cache_after.created_by, founder.id);
    assert_eq!(cache_after.display_name, "Store");
    assert!(
        test_db
            .rows::<ECacheUser>()
            .await
            .iter()
            .all(|member| member.user != founder.id)
    );
    assert_eq!(connection_after.created_by, founder.id);
    test_db.finish().await;
}

#[tokio::test]
async fn apply_state_with_empty_created_by_for_new_project_task_or_cache_fails_and_creates_no_row()
{
    let test_db = TestDatabase::create().await;
    let db = test_db.connection();
    let founder =
        insert_publicly_registered_account_with_password_hash(&db, "founder", "founder@self.io")
            .await;
    MProject {
        id: ProjectId::now_v7(),
        name: "acme".into(),
        created_by: founder.id,
        ..rows::project()
    }
    .into_active_model()
    .insert(&db)
    .await
    .unwrap();
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();
    state.write_project_private_key("fresh");
    state.write("cache", "store", "signing_key", &"A".repeat(88));

    for state_json in [
        r#"{ "projects": { "fresh": {
            "name": "fresh", "display_name": "Fresh", "private_key_file": "k",
            "public": false, "created_by": ""
        } } }"#,
        r#"{ "tasks": { "build": {
            "name": "build", "project": "acme", "display_name": "Build",
            "repository": "https://github.com/test/repo", "created_by": ""
        } } }"#,
        r#"{ "caches": { "store": {
            "name": "store", "display_name": "Store", "signing_key_file": "k",
            "public": false, "created_by": ""
        } } }"#,
    ] {
        let error = state
            .apply(&db, state_json, false)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("created_by is empty"), "{error}");
    }
    assert_eq!(test_db.rows::<EProject>().await.len(), 1);
    assert!(test_db.rows::<ETask>().await.is_empty());
    assert!(test_db.rows::<ECache>().await.is_empty());
    test_db.finish().await;
}
