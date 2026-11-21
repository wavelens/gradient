/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::is_unique_violation;
use anyhow::{Context, Result};
use gradient_core::{ExternalIdentity, LoginMethods, LoginRefusal};
use gradient_db::WebDb;
use gradient_types::*;
use sea_orm::{ActiveValue::Set, DatabaseTransaction, TransactionTrait};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PasswordLogin {
    On,
    Off,
}

impl PasswordLogin {
    pub fn of(methods: &LoginMethods) -> Self {
        if methods.password_login() {
            Self::On
        } else {
            Self::Off
        }
    }
}

pub struct Candidate {
    pub user: MUser,
    pub has_identity: bool,
}

impl Candidate {
    /// A verified email address can claim a password account once no one can sign in with the password. The claim can remove the previous owner's password, sessions and API keys.
    fn is_claimable(&self, password_login: PasswordLogin) -> bool {
        let password_blocks = self.user.password.is_some() && password_login == PasswordLogin::On;
        !password_blocks && !self.has_identity
    }
}

pub enum LoginDecision {
    Update(MUser),
    Claim(MUser),
    Create,
    Refuse(LoginRefusal),
}

pub fn decide(
    linked: Option<MUser>,
    by_username: Option<Candidate>,
    by_email: Option<Candidate>,
    password_login: PasswordLogin,
) -> LoginDecision {
    if let Some(user) = linked {
        return if user.active {
            LoginDecision::Update(user)
        } else {
            LoginDecision::Refuse(LoginRefusal::Deactivated)
        };
    }

    let candidate = match (by_email, by_username) {
        (Some(by_email), _) => Some(by_email),
        (None, Some(_)) => return LoginDecision::Refuse(LoginRefusal::UsernameTaken),
        (None, None) => None,
    };

    match candidate {
        None => LoginDecision::Create,
        Some(candidate) if !candidate.is_claimable(password_login) => {
            LoginDecision::Refuse(LoginRefusal::AccountExists)
        }
        Some(candidate) if !candidate.user.active => {
            LoginDecision::Refuse(LoginRefusal::Deactivated)
        }
        Some(candidate) => LoginDecision::Claim(candidate.user),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("a concurrent sign-in wrote the same user or identity first")]
struct ConcurrentSignIn;

/// A concurrent first sign-in of the same account loses the unique-index race; the retry then finds the winner's rows.
pub async fn sign_in_external(
    db: &WebDb,
    identity: ExternalIdentity,
    password_login: PasswordLogin,
) -> Result<MUser> {
    match attempt_sign_in(db, &identity, password_login).await {
        Err(e) if e.is::<ConcurrentSignIn>() => attempt_sign_in(db, &identity, password_login)
            .await
            .map_err(|e| {
                if e.is::<ConcurrentSignIn>() {
                    LoginRefusal::AccountExists.into()
                } else {
                    e
                }
            }),
        outcome => outcome,
    }
}

async fn attempt_sign_in(
    db: &WebDb,
    identity: &ExternalIdentity,
    password_login: PasswordLogin,
) -> Result<MUser> {
    let tx = db
        .inner()
        .begin()
        .await
        .context("start external login transaction")?;

    let linked = match gradient_db::user_identities::user_by_identity(
        &tx,
        &identity.issuer,
        &identity.subject,
    )
    .await
    .context("find linked user")?
    {
        Some(user) => Some(lock_user_row_and_recheck_identity_link(&tx, user.id, identity).await?),
        None => None,
    };

    let (by_username, by_email) = if linked.is_some() {
        (None, None)
    } else {
        let by_email = match &identity.email {
            Some(email) if !email.is_empty() => {
                gradient_db::users::by_email_ignoring_case(&tx, email)
                    .await
                    .context("find user by email")?
            }
            _ => None,
        };
        let by_username = if by_email.is_some() {
            None
        } else {
            gradient_db::users::by_username(&tx, &identity.username)
                .await
                .context("find user by username")?
        };
        (
            candidate(&tx, by_username, None).await?,
            candidate(&tx, by_email, identity.email.as_deref()).await?,
        )
    };

    let user = match decide(linked, by_username, by_email, password_login) {
        LoginDecision::Refuse(refusal) => return Err(refusal.into()),
        LoginDecision::Update(user) => touch(&tx, user, identity).await?,
        LoginDecision::Claim(user) => {
            gradient_db::users::revoke_previous_account_holder_access(&tx, user.id)
                .await
                .context("Revoke access for the previous account owner")?;
            bind(&tx, user.id, identity).await?;
            touch(&tx, user, identity).await?
        }
        LoginDecision::Create => {
            let user = create(&tx, identity).await?;
            bind(&tx, user.id, identity).await?;
            user
        }
    };

    if let Some(groups) = &identity.groups {
        let savepoint = tx.begin().await.context("start group sync savepoint")?;
        match gradient_db::teams::members::sync_group_memberships(&savepoint, user.id, groups).await
        {
            Ok(()) => savepoint
                .commit()
                .await
                .context("release group sync savepoint")?,
            Err(e) => {
                tracing::warn!(error = %e, username = %user.username, "Failed to sync group team memberships");
                savepoint
                    .rollback()
                    .await
                    .context("roll back group sync savepoint")?;
            }
        }
    }

    tx.commit()
        .await
        .context("commit external login transaction")?;
    Ok(user)
}

async fn lock_user_row_and_recheck_identity_link(
    tx: &DatabaseTransaction,
    user: UserId,
    identity: &ExternalIdentity,
) -> Result<MUser> {
    let locked = gradient_db::users::lock_user_row_for_update(tx, user)
        .await
        .context("Lock the user row for the linked user.")?;
    let still_linked =
        gradient_db::user_identities::user_by_identity(tx, &identity.issuer, &identity.subject)
            .await
            .context("Find linked user under the row lock on the user.")?
            .is_some_and(|linked| linked.id == user);
    match locked {
        Some(locked) if still_linked => Ok(locked),
        _ => Err(LoginRefusal::AccountExists.into()),
    }
}

async fn candidate(
    tx: &DatabaseTransaction,
    user: Option<MUser>,
    email_to_match_locked_row: Option<&str>,
) -> Result<Option<Candidate>> {
    let Some(user) = user else {
        return Ok(None);
    };
    let Some(user) = gradient_db::users::lock_user_row_for_update(tx, user.id)
        .await
        .context("Lock the user row for the matching user.")?
    else {
        return Ok(None);
    };
    if let Some(email) = email_to_match_locked_row {
        let still_matches = gradient_db::users::by_email_ignoring_case(tx, email)
            .await
            .context("Find user by email address under the row lock on the user.")?
            .is_some_and(|matching| matching.id == user.id);
        if !still_matches {
            return Err(LoginRefusal::AccountExists.into());
        }
    }
    let has_identity = !gradient_db::user_identities::of_user(tx, user.id)
        .await
        .context("load identities of a matching user")?
        .is_empty();
    Ok(Some(Candidate { user, has_identity }))
}

async fn touch(
    tx: &DatabaseTransaction,
    user: MUser,
    identity: &ExternalIdentity,
) -> Result<MUser> {
    let signed_in_account_id_before_activation = user.id;
    let mut auser: AUser = user.into();
    auser.last_login_at = Set(gradient_types::now());
    if identity.refresh_profile {
        if let Some(email) = identity
            .email
            .as_ref()
            .filter(|email| input::validate_email(email).is_ok())
            && !gradient_db::users::by_email_ignoring_case(tx, email)
                .await
                .context("find account by email address from identity provider")?
                .is_some_and(|account_with_email_address| {
                    account_with_email_address.id != signed_in_account_id_before_activation
                })
        {
            auser.email = Set(email.clone());
        }
        if let Some(name) = identity
            .name
            .as_ref()
            .filter(|name| input::validate_display_name(name).is_ok())
        {
            auser.name = Set(name.clone());
        }
    }
    gradient_db::rows::update::<EUser, _>(tx, auser)
        .await
        .context("update external login user")
}

async fn create(tx: &DatabaseTransaction, identity: &ExternalIdentity) -> Result<MUser> {
    let display_name = input::choose_account_display_name_from_identity_provider(
        identity.name.as_deref(),
        &identity.username,
    );
    let new_user = gradient_db::users::ValidatedNewUser::new(
        AccountOrigin::CreatedOnFirstSignInWithExternalLoginProvider,
        &identity.username,
        &display_name,
        identity
            .email
            .as_deref()
            .filter(|email| input::validate_email(email).is_ok()),
    )
    .map_err(|_| LoginRefusal::UnusableUsername)?
    .with_email_verified(true);
    gradient_db::users::insert_user_account(tx, new_user)
        .await
        .map_err(refuse_on_conflict("create user"))
}

async fn bind(tx: &DatabaseTransaction, user: UserId, identity: &ExternalIdentity) -> Result<()> {
    gradient_db::user_identities::insert(tx, user, &identity.issuer, &identity.subject)
        .await
        .map_err(refuse_on_conflict("bind external identity"))?;
    Ok(())
}

fn refuse_on_conflict(stage: &'static str) -> impl FnOnce(sea_orm::DbErr) -> anyhow::Error {
    move |e| {
        if is_unique_violation(&e) {
            ConcurrentSignIn.into()
        } else {
            anyhow::Error::new(e).context(stage)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active() -> MUser {
        MUser {
            username: "alice".into(),
            email: "alice@example.com".into(),
            active: true,
            ..Default::default()
        }
    }

    fn unclaimed(user: MUser) -> Option<Candidate> {
        Some(Candidate {
            user,
            has_identity: false,
        })
    }

    #[test]
    fn linked_active_user_updates() {
        let decision = decide(Some(active()), None, None, PasswordLogin::On);
        assert!(matches!(decision, LoginDecision::Update(_)));
    }

    #[test]
    fn linked_deactivated_user_refuses() {
        let user = MUser {
            active: false,
            ..active()
        };
        let decision = decide(Some(user), None, None, PasswordLogin::On);
        assert!(matches!(
            decision,
            LoginDecision::Refuse(LoginRefusal::Deactivated)
        ));
    }

    #[test]
    fn external_login_username_matches_unlinked_passwordless_account_with_other_email_is_refused_as_username_taken()
     {
        let other = MUser {
            email: "someone-else@example.com".into(),
            ..active()
        };
        let decision = decide(None, unclaimed(other), None, PasswordLogin::On);
        assert!(matches!(
            decision,
            LoginDecision::Refuse(LoginRefusal::UsernameTaken)
        ));
    }

    #[test]
    fn verified_email_claims_unclaimed_match() {
        let decision = decide(None, None, unclaimed(active()), PasswordLogin::On);
        assert!(matches!(decision, LoginDecision::Claim(_)));
    }

    #[test]
    fn claimed_collision_refuses() {
        let with_password = MUser {
            password: Some("$argon2id$...".into()),
            ..active()
        };
        let decision = decide(
            None,
            unclaimed(with_password.clone()),
            unclaimed(with_password),
            PasswordLogin::On,
        );
        assert!(matches!(
            decision,
            LoginDecision::Refuse(LoginRefusal::AccountExists)
        ));
    }

    #[test]
    fn candidate_with_identity_refuses() {
        let bound = Some(Candidate {
            user: active(),
            has_identity: true,
        });
        let decision = decide(None, None, bound, PasswordLogin::On);
        assert!(matches!(
            decision,
            LoginDecision::Refuse(LoginRefusal::AccountExists)
        ));
    }

    #[test]
    fn verified_email_claims_password_account_when_password_login_off() {
        let with_password = MUser {
            password: Some("$argon2id$...".into()),
            ..active()
        };
        let decision = decide(None, None, unclaimed(with_password), PasswordLogin::Off);
        assert!(matches!(decision, LoginDecision::Claim(_)));
    }

    #[test]
    fn decide_returns_username_taken_for_username_match_when_password_login_is_off() {
        let state_superuser = MUser {
            email: "root@example.com".into(),
            superuser: true,
            managed: true,
            account_origin: AccountOrigin::DeclaredInStateFile,
            ..active()
        };
        let decision = decide(None, unclaimed(state_superuser), None, PasswordLogin::Off);
        assert!(matches!(
            decision,
            LoginDecision::Refuse(LoginRefusal::UsernameTaken)
        ));
    }

    #[test]
    fn decide_returns_username_taken_for_username_match_with_password_account_when_password_login_is_on()
     {
        let other = MUser {
            email: "someone-else@example.com".into(),
            password: Some("$argon2id$...".into()),
            ..active()
        };
        let decision = decide(None, unclaimed(other), None, PasswordLogin::On);
        assert!(matches!(
            decision,
            LoginDecision::Refuse(LoginRefusal::UsernameTaken)
        ));
    }

    #[test]
    fn no_match_creates() {
        let decision = decide(None, None, None, PasswordLogin::On);
        assert!(matches!(decision, LoginDecision::Create));
    }
}

#[cfg(test)]
mod sign_in_against_database_tests {
    use super::*;
    use gradient_db::users::{ValidatedNewUser, insert_user_account};
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;
    use sea_orm::{DatabaseConnection, IntoActiveModel};

    async fn insert_registered_account_with_password_session_and_api_key(
        db: &DatabaseConnection,
        username: &str,
        email: &str,
    ) -> MUser {
        let user = insert_user_account(
            db,
            ValidatedNewUser::new(
                AccountOrigin::PublicRegistration,
                username,
                username,
                Some(email),
            )
            .unwrap()
            .with_password_hash(Some("$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA".into()))
            .with_email_verified(true),
        )
        .await
        .unwrap();
        gradient_db::rows::insert::<ESession, _>(
            db,
            MSession {
                user_id: user.id,
                ..rows::live_session(SessionId::now_v7())
            },
        )
        .await
        .unwrap();
        gradient_db::rows::insert::<EApi, _>(
            db,
            MApi {
                id: ApiId::now_v7(),
                owned_by: user.id,
                name: "laptop".into(),
                key: "0".repeat(64),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        user
    }

    fn identity(username: &str, email: Option<&str>) -> ExternalIdentity {
        ExternalIdentity {
            issuer: "https://idp.example.com".into(),
            subject: "subject-1".into(),
            username: username.into(),
            email: email.map(str::to_owned),
            name: None,
            groups: None,
            refresh_profile: false,
        }
    }

    fn refusal(result: Result<MUser>) -> LoginRefusal {
        match result {
            Ok(user) => panic!("sign-in succeeded as {}", user.username),
            Err(e) => e.downcast::<LoginRefusal>().expect("a login refusal"),
        }
    }

    #[tokio::test]
    async fn external_login_with_verified_email_claims_existing_password_account_and_removes_previous_holder_password_sessions_api_keys_and_ssh_keys()
     {
        let test_db = TestDatabase::create().await;
        let db = test_db.connection();
        let squatter = insert_registered_account_with_password_session_and_api_key(
            &db,
            "squatter",
            "victim@corp.io",
        )
        .await;
        gradient_db::rows::insert::<EUserSshKey, _>(
            &db,
            MUserSshKey {
                id: UserSshKeyId::now_v7(),
                user: squatter.id,
                name: "laptop".into(),
                public_key: "ssh-ed25519 AAAA laptop".into(),
                fingerprint: "SHA256:laptop".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(test_db.rows::<EUserSshKey>().await.len(), 1);

        let user = sign_in_external(
            &WebDb::new(db.clone()),
            identity("victim", Some("victim@corp.io")),
            PasswordLogin::Off,
        )
        .await
        .unwrap();

        assert_eq!(user.id, squatter.id);
        assert_eq!(
            test_db.find::<EUser>(squatter.id).await.unwrap().password,
            None
        );
        assert!(test_db.rows::<ESession>().await.is_empty());
        assert!(test_db.rows::<EApi>().await.is_empty());
        assert!(test_db.rows::<EUserSshKey>().await.is_empty());
        assert_eq!(test_db.rows::<EUserIdentity>().await.len(), 1);
        test_db.finish().await;
    }

    #[tokio::test]
    async fn external_login_claim_removes_previous_holder_sign_in_group_sync_memberships_and_keeps_api_and_scim_memberships()
     {
        let test_db = TestDatabase::create().await;
        let db = test_db.connection();
        let squatter = insert_registered_account_with_password_session_and_api_key(
            &db,
            "squatter",
            "victim@corp.io",
        )
        .await;
        let mut scim_provisioned_user_active_model = squatter.into_active_model();
        scim_provisioned_user_active_model.account_origin =
            Set(AccountOrigin::ScimProvisionedByIdentityProvider);
        let squatter =
            gradient_db::rows::update::<EUser, _>(&db, scim_provisioned_user_active_model)
                .await
                .unwrap();
        let mut teams = Vec::new();
        for (name, oidc_group, scim_group) in [
            ("a", None, None),
            ("b", None, None),
            ("c", None, Some("c-scim")),
            ("d", Some("c-group"), None),
        ] {
            let team = gradient_db::rows::insert::<ETeam, _>(
                &db,
                MTeam {
                    id: TeamId::now_v7(),
                    name: name.into(),
                    oidc_group: oidc_group.map(str::to_owned),
                    scim_group: scim_group.map(str::to_owned),
                    created_by: None,
                    ..rows::team()
                },
            )
            .await
            .unwrap();
            teams.push(team.id);
        }
        for (team, source) in [
            (teams[0], TeamMemberSource::SignInGroupSync),
            (teams[1], TeamMemberSource::Api),
            (teams[2], TeamMemberSource::ScimGroup),
        ] {
            gradient_db::rows::insert::<ETeamUser, _>(
                &db,
                MTeamUser {
                    team,
                    user: squatter.id,
                    source,
                    ..rows::team_member(TeamRole::Member)
                },
            )
            .await
            .unwrap();
        }

        let user = sign_in_external(
            &WebDb::new(db.clone()),
            ExternalIdentity {
                groups: Some(vec!["c-group".into()]),
                ..identity("victim", Some("victim@corp.io"))
            },
            PasswordLogin::Off,
        )
        .await
        .unwrap();

        assert_eq!(user.id, squatter.id);
        let mut memberships: Vec<(TeamId, TeamMemberSource)> = test_db
            .rows::<ETeamUser>()
            .await
            .into_iter()
            .map(|membership| (membership.team, membership.source))
            .collect();
        memberships.sort_by_key(|(team, _)| teams.iter().position(|id| id == team));
        assert_eq!(
            memberships,
            vec![
                (teams[1], TeamMemberSource::Api),
                (teams[2], TeamMemberSource::ScimGroup),
                (teams[3], TeamMemberSource::SignInGroupSync),
            ]
        );
        test_db.finish().await;
    }

    #[tokio::test]
    async fn external_login_claim_with_failing_group_sync_keeps_identity_link_and_removal_of_previous_holder_access_and_adds_no_membership()
     {
        let test_db = TestDatabase::create().await;
        let db = test_db.connection();
        let squatter = insert_registered_account_with_password_session_and_api_key(
            &db,
            "squatter",
            "victim@corp.io",
        )
        .await;
        let mut teams = Vec::new();
        for (name, oidc_group) in [("a", None), ("d", Some("c-group"))] {
            let team = gradient_db::rows::insert::<ETeam, _>(
                &db,
                MTeam {
                    id: TeamId::now_v7(),
                    name: name.into(),
                    oidc_group: oidc_group.map(str::to_owned),
                    created_by: None,
                    ..rows::team()
                },
            )
            .await
            .unwrap();
            teams.push(team.id);
        }
        gradient_db::rows::insert::<ETeamUser, _>(
            &db,
            MTeamUser {
                team: teams[0],
                user: squatter.id,
                source: TeamMemberSource::SignInGroupSync,
                ..rows::team_member(TeamRole::Member)
            },
        )
        .await
        .unwrap();
        test_db
            .execute(
                "ALTER TABLE team_user ADD CONSTRAINT refuse_new_sign_in_group_sync_rows \
                 CHECK (source <> 3) NOT VALID",
            )
            .await;

        let user = sign_in_external(
            &WebDb::new(db.clone()),
            ExternalIdentity {
                groups: Some(vec!["c-group".into()]),
                ..identity("victim", Some("victim@corp.io"))
            },
            PasswordLogin::Off,
        )
        .await
        .unwrap();

        assert_eq!(user.id, squatter.id);
        assert_eq!(test_db.rows::<EUserIdentity>().await.len(), 1);
        assert!(test_db.rows::<ESession>().await.is_empty());
        assert!(test_db.rows::<EApi>().await.is_empty());
        assert!(test_db.rows::<ETeamUser>().await.is_empty());
        test_db.finish().await;
    }

    #[tokio::test]
    async fn external_login_with_verified_email_differing_only_in_letter_case_claims_existing_password_account_and_removes_previous_holder_password_sessions_and_api_keys()
     {
        let test_db = TestDatabase::create().await;
        let db = test_db.connection();
        let squatter = insert_registered_account_with_password_session_and_api_key(
            &db,
            "squatter",
            "victim@corp.io",
        )
        .await;
        assert!(squatter.password.is_some());

        let user = sign_in_external(
            &WebDb::new(db.clone()),
            identity("victim", Some("Victim@Corp.IO")),
            PasswordLogin::Off,
        )
        .await
        .unwrap();

        assert_eq!(user.id, squatter.id);
        assert_eq!(test_db.rows::<EUser>().await.len(), 1);
        assert_eq!(
            test_db.find::<EUser>(squatter.id).await.unwrap().password,
            None
        );
        assert!(test_db.rows::<ESession>().await.is_empty());
        assert!(test_db.rows::<EApi>().await.is_empty());
        assert_eq!(test_db.rows::<EUserIdentity>().await.len(), 1);
        test_db.finish().await;
    }

    #[tokio::test]
    async fn profile_refresh_skips_email_and_display_name_that_fail_validation() {
        let test_db = TestDatabase::create().await;
        let db = WebDb::new(test_db.connection());
        let first = sign_in_external(
            &db,
            identity("alice", Some("alice@idp.io")),
            PasswordLogin::On,
        )
        .await
        .unwrap();

        let refreshed = sign_in_external(
            &db,
            ExternalIdentity {
                email: Some("not an email".into()),
                name: Some("<script>".into()),
                refresh_profile: true,
                ..identity("alice", None)
            },
            PasswordLogin::On,
        )
        .await
        .unwrap();

        assert_eq!(refreshed.email, first.email);
        assert_eq!(refreshed.name, first.name);
        test_db.finish().await;
    }

    #[tokio::test]
    async fn external_login_profile_refresh_with_email_owned_by_another_account_succeeds_and_keeps_both_accounts_own_emails()
     {
        let test_db = TestDatabase::create().await;
        let db = WebDb::new(test_db.connection());
        let other = insert_registered_account_with_password_session_and_api_key(
            &test_db.connection(),
            "bob",
            "bob@corp.io",
        )
        .await;
        let first = sign_in_external(
            &db,
            identity("alice", Some("alice@idp.io")),
            PasswordLogin::On,
        )
        .await
        .unwrap();

        let refreshed = sign_in_external(
            &db,
            ExternalIdentity {
                refresh_profile: true,
                ..identity("alice", Some("Bob@corp.io"))
            },
            PasswordLogin::On,
        )
        .await
        .unwrap();

        assert_eq!(refreshed.id, first.id);
        assert_eq!(refreshed.email, "alice@idp.io");
        assert_eq!(
            test_db.find::<EUser>(other.id).await.unwrap().email,
            "bob@corp.io"
        );
        test_db.finish().await;
    }

    #[tokio::test]
    async fn sign_in_with_letter_case_variant_of_taken_username_is_refused() {
        let test_db = TestDatabase::create().await;
        let db = test_db.connection();
        insert_registered_account_with_password_session_and_api_key(&db, "alice", "alice@self.io")
            .await;

        let result = sign_in_external(
            &WebDb::new(db.clone()),
            identity("Alice", Some("alice@idp.io")),
            PasswordLogin::Off,
        )
        .await;

        assert!(matches!(refusal(result), LoginRefusal::UsernameTaken));
        assert_eq!(test_db.rows::<EUser>().await.len(), 1);
        assert!(test_db.rows::<EUserIdentity>().await.is_empty());
        test_db.finish().await;
    }

    #[tokio::test]
    async fn sign_in_with_spaced_or_look_alike_variant_of_taken_username_is_refused_as_unusable() {
        let test_db = TestDatabase::create().await;
        let db = test_db.connection();
        insert_registered_account_with_password_session_and_api_key(&db, "alice", "alice@self.io")
            .await;

        for username in [" alice ", "\u{430}lice"] {
            let result = sign_in_external(
                &WebDb::new(db.clone()),
                identity(username, None),
                PasswordLogin::On,
            )
            .await;
            assert!(matches!(refusal(result), LoginRefusal::UnusableUsername));
        }

        assert_eq!(test_db.rows::<EUser>().await.len(), 1);
        test_db.finish().await;
    }

    #[tokio::test]
    async fn external_sign_in_is_refused_when_identity_link_appears_while_waiting_for_user_row_lock()
     {
        let test_db = TestDatabase::create().await;
        let db = test_db.connection();
        let holder = insert_registered_account_with_password_session_and_api_key(
            &db,
            "holder",
            "victim@corp.io",
        )
        .await;
        let connecting = db.begin().await.unwrap();
        gradient_db::users::lock_user_row_for_update(&connecting, holder.id)
            .await
            .unwrap();

        let web_db = WebDb::new(db.clone());
        let claim = sign_in_external(
            &web_db,
            identity("victim", Some("victim@corp.io")),
            PasswordLogin::Off,
        );
        tokio::pin!(claim);
        let waiting = tokio::time::timeout(std::time::Duration::from_millis(500), &mut claim).await;
        gradient_db::user_identities::insert(
            &connecting,
            holder.id,
            "https://other-idp.example.com",
            "holder",
        )
        .await
        .unwrap();
        connecting.commit().await.unwrap();

        assert!(waiting.is_err());
        assert!(matches!(refusal(claim.await), LoginRefusal::AccountExists));
        assert!(
            test_db
                .find::<EUser>(holder.id)
                .await
                .unwrap()
                .password
                .is_some()
        );
        assert_eq!(test_db.rows::<ESession>().await.len(), 1);
        assert_eq!(test_db.rows::<EUserIdentity>().await.len(), 1);
        test_db.finish().await;
    }

    #[tokio::test]
    async fn external_sign_in_is_refused_when_account_takeover_deletes_identity_link_while_sign_in_waits_for_user_row_lock_and_email_display_name_stay_unchanged_and_sign_in_group_sync_memberships_are_removed()
     {
        let test_db = TestDatabase::create().await;
        let db = test_db.connection();
        let web_db = WebDb::new(db.clone());
        gradient_db::rows::insert::<ETeam, _>(
            &db,
            MTeam {
                oidc_group: Some("ops-admins".into()),
                created_by: None,
                ..rows::team()
            },
        )
        .await
        .unwrap();
        let previous_holder = sign_in_external(
            &web_db,
            ExternalIdentity {
                groups: Some(vec!["ops-admins".into()]),
                ..identity("ops", Some("old@corp.io"))
            },
            PasswordLogin::On,
        )
        .await
        .unwrap();
        assert_eq!(test_db.rows::<ETeamUser>().await.len(), 1);

        let takeover = db.begin().await.unwrap();
        let locked = gradient_db::users::lock_user_row_for_update(&takeover, previous_holder.id)
            .await
            .unwrap()
            .unwrap();

        let sign_in = sign_in_external(
            &web_db,
            ExternalIdentity {
                name: Some("Old Holder".into()),
                groups: Some(vec!["ops-admins".into()]),
                refresh_profile: true,
                ..identity("ops", Some("old@corp.io"))
            },
            PasswordLogin::On,
        );
        tokio::pin!(sign_in);
        let waiting =
            tokio::time::timeout(std::time::Duration::from_millis(500), &mut sign_in).await;
        gradient_db::users::delete_account_identity_links_and_clear_scim_external_id(
            &takeover,
            previous_holder.id,
        )
        .await
        .unwrap();
        gradient_db::users::revoke_previous_account_holder_access(&takeover, previous_holder.id)
            .await
            .unwrap();
        let mut new_holder: AUser = locked.into();
        new_holder.email = Set("new@corp.io".into());
        new_holder.name = Set("New Holder".into());
        gradient_db::rows::update::<EUser, _>(&takeover, new_holder)
            .await
            .unwrap();
        takeover.commit().await.unwrap();

        assert!(waiting.is_err());
        assert!(matches!(
            refusal(sign_in.await),
            LoginRefusal::AccountExists
        ));
        let account = test_db.find::<EUser>(previous_holder.id).await.unwrap();
        assert_eq!(account.email, "new@corp.io");
        assert_eq!(account.name, "New Holder");
        assert!(test_db.rows::<ETeamUser>().await.is_empty());
        assert!(test_db.rows::<EUserIdentity>().await.is_empty());
        assert!(test_db.rows::<ESession>().await.is_empty());
        test_db.finish().await;
    }

    #[tokio::test]
    async fn external_sign_in_is_refused_when_account_email_changes_while_waiting_for_user_row_lock()
     {
        assert_external_sign_in_refused_after_claimed_account_email_changes_while_locked(
            "old@corp.io",
            "old@corp.io",
            "new@corp.io",
        )
        .await;
    }

    #[tokio::test]
    async fn external_sign_in_is_refused_when_account_email_changes_while_waiting_for_user_row_lock_to_greek_uppercase_ending_in_capital_sigma_that_rust_lowercases_to_final_sigma_equal_to_identity_email_but_postgres_lower_maps_to_medial_sigma_not_equal()
     {
        assert_external_sign_in_refused_after_claimed_account_email_changes_while_locked(
            "\u{3b1}\u{3c2}@corp.io",
            "\u{391}\u{3a3}@corp.io",
            "\u{391}\u{3a3}@corp.io",
        )
        .await;
    }

    async fn assert_external_sign_in_refused_after_claimed_account_email_changes_while_locked(
        stored_email: &str,
        identity_email: &str,
        changed_email: &str,
    ) {
        let test_db = TestDatabase::create().await;
        let db = test_db.connection();
        let holder = insert_registered_account_with_password_session_and_api_key(
            &db,
            "holder",
            stored_email,
        )
        .await;
        let email_change = db.begin().await.unwrap();
        let locked = gradient_db::users::lock_user_row_for_update(&email_change, holder.id)
            .await
            .unwrap()
            .unwrap();

        let web_db = WebDb::new(db.clone());
        let claim = sign_in_external(
            &web_db,
            identity("claimant", Some(identity_email)),
            PasswordLogin::Off,
        );
        tokio::pin!(claim);
        let waiting = tokio::time::timeout(std::time::Duration::from_millis(500), &mut claim).await;
        let mut changed: AUser = locked.into();
        changed.email = Set(changed_email.into());
        gradient_db::rows::update::<EUser, _>(&email_change, changed)
            .await
            .unwrap();
        email_change.commit().await.unwrap();

        assert!(waiting.is_err());
        assert!(matches!(refusal(claim.await), LoginRefusal::AccountExists));
        let account = test_db.find::<EUser>(holder.id).await.unwrap();
        assert_eq!(account.email, changed_email);
        assert!(account.password.is_some());
        assert_eq!(test_db.rows::<ESession>().await.len(), 1);
        assert!(test_db.rows::<EUserIdentity>().await.is_empty());
        test_db.finish().await;
    }
}
