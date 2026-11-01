/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::is_unique_violation;
use anyhow::{Context, Result};
use gradient_core::{ExternalIdentity, LoginMethods, LoginRefusal, MatchRule};
use gradient_db::WebDb;
use gradient_state::PendingProjectMemberships;
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
    /// A verified email may claim a password account once passwords no longer sign anyone in.
    fn is_claimable(&self, rule: MatchRule, password_login: PasswordLogin) -> bool {
        let password_blocks = self.user.password.is_some()
            && !(rule == MatchRule::VerifiedEmail && password_login == PasswordLogin::Off);
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
    identity: &ExternalIdentity,
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

    let candidate = match identity.match_rule {
        MatchRule::UsernameOrEmail => by_username.or(by_email),
        MatchRule::VerifiedEmail => match (by_email, by_username) {
            (Some(by_email), _) => Some(by_email),
            (None, Some(_)) => return LoginDecision::Refuse(LoginRefusal::UsernameTaken),
            (None, None) => None,
        },
    };

    match candidate {
        None => LoginDecision::Create,
        Some(candidate) if !candidate.is_claimable(identity.match_rule, password_login) => {
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
    pending: &PendingProjectMemberships,
    identity: ExternalIdentity,
    password_login: PasswordLogin,
) -> Result<MUser> {
    match attempt_sign_in(db, pending, &identity, password_login).await {
        Err(e) if e.is::<ConcurrentSignIn>() => {
            attempt_sign_in(db, pending, &identity, password_login)
                .await
                .map_err(|e| {
                    if e.is::<ConcurrentSignIn>() {
                        LoginRefusal::AccountExists.into()
                    } else {
                        e
                    }
                })
        }
        outcome => outcome,
    }
}

async fn attempt_sign_in(
    db: &WebDb,
    pending: &PendingProjectMemberships,
    identity: &ExternalIdentity,
    password_login: PasswordLogin,
) -> Result<MUser> {
    let tx = db
        .inner()
        .begin()
        .await
        .context("start external login transaction")?;

    let linked =
        gradient_db::user_identities::user_by_identity(&tx, &identity.issuer, &identity.subject)
            .await
            .context("find linked user")?;

    let (by_username, by_email) = if linked.is_some() {
        (None, None)
    } else {
        let by_username = gradient_db::users::by_username(&tx, &identity.username)
            .await
            .context("find user by username")?;
        let by_email = match &identity.email {
            Some(email) if !email.is_empty() => {
                gradient_db::users::by_email_ignoring_case(&tx, email)
                    .await
                    .context("find user by email")?
            }
            _ => None,
        };
        (
            candidate(&tx, by_username).await?,
            candidate(&tx, by_email).await?,
        )
    };

    let user = match decide(identity, linked, by_username, by_email, password_login) {
        LoginDecision::Refuse(refusal) => return Err(refusal.into()),
        LoginDecision::Update(user) => touch(&tx, user, identity).await?,
        LoginDecision::Claim(user) => {
            bind(&tx, user.id, identity).await?;
            let user = touch(&tx, user, identity).await?;
            apply_pending(&tx, pending, &user).await;
            user
        }
        LoginDecision::Create => {
            let user = create(&tx, identity).await?;
            bind(&tx, user.id, identity).await?;
            apply_pending(&tx, pending, &user).await;
            user
        }
    };

    if let Some(groups) = &identity.groups
        && let Err(e) =
            gradient_db::teams::members::sync_group_memberships(&tx, user.id, groups).await
    {
        tracing::warn!(error = %e, username = %user.username, "Failed to sync group team memberships");
    }

    tx.commit()
        .await
        .context("commit external login transaction")?;
    Ok(user)
}

async fn candidate(tx: &DatabaseTransaction, user: Option<MUser>) -> Result<Option<Candidate>> {
    let Some(user) = user else {
        return Ok(None);
    };
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
    let mut auser: AUser = user.into();
    auser.last_login_at = Set(gradient_types::now());
    if identity.refresh_profile {
        if let Some(email) = &identity.email {
            auser.email = Set(email.clone());
        }
        if let Some(name) = &identity.name {
            auser.name = Set(name.clone());
        }
    }
    gradient_db::rows::update::<EUser, _>(tx, auser)
        .await
        .context("update external login user")
}

async fn create(tx: &DatabaseTransaction, identity: &ExternalIdentity) -> Result<MUser> {
    gradient_db::rows::insert::<EUser, _>(
        tx,
        MUser {
            id: UserId::now_v7(),
            username: identity.username.clone(),
            name: identity
                .name
                .clone()
                .unwrap_or_else(|| identity.username.clone()),
            email: identity.email.clone().unwrap_or_default(),
            last_login_at: gradient_types::now(),
            created_at: gradient_types::now(),
            email_verified: true,
            active: true,
            ..Default::default()
        },
    )
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

async fn apply_pending(
    tx: &DatabaseTransaction,
    pending: &PendingProjectMemberships,
    user: &MUser,
) {
    if let Err(e) =
        gradient_state::apply_pending_project_memberships(tx, pending, &user.username, user.id)
            .await
    {
        tracing::warn!(error = %e, username = %user.username,
            "Failed to apply pending state-managed project memberships");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(match_rule: MatchRule) -> ExternalIdentity {
        ExternalIdentity {
            issuer: "https://provider.example.com".into(),
            subject: "42".into(),
            username: "alice".into(),
            email: Some("alice@example.com".into()),
            name: None,
            groups: None,
            match_rule,
            refresh_profile: false,
        }
    }

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
        let decision = decide(
            &identity(MatchRule::VerifiedEmail),
            Some(active()),
            None,
            None,
            PasswordLogin::On,
        );
        assert!(matches!(decision, LoginDecision::Update(_)));
    }

    #[test]
    fn linked_deactivated_user_refuses() {
        let user = MUser {
            active: false,
            ..active()
        };
        let decision = decide(
            &identity(MatchRule::VerifiedEmail),
            Some(user),
            None,
            None,
            PasswordLogin::On,
        );
        assert!(matches!(
            decision,
            LoginDecision::Refuse(LoginRefusal::Deactivated)
        ));
    }

    #[test]
    fn oidc_claims_unclaimed_username_match() {
        let decision = decide(
            &identity(MatchRule::UsernameOrEmail),
            None,
            unclaimed(active()),
            None,
            PasswordLogin::On,
        );
        assert!(matches!(decision, LoginDecision::Claim(_)));
    }

    #[test]
    fn verified_email_claims_unclaimed_match() {
        let decision = decide(
            &identity(MatchRule::VerifiedEmail),
            None,
            None,
            unclaimed(active()),
            PasswordLogin::On,
        );
        assert!(matches!(decision, LoginDecision::Claim(_)));
    }

    #[test]
    fn verified_email_refuses_username_only_clash_with_unclaimed() {
        let other = MUser {
            email: "someone-else@example.com".into(),
            ..active()
        };
        let decision = decide(
            &identity(MatchRule::VerifiedEmail),
            None,
            unclaimed(other),
            None,
            PasswordLogin::On,
        );
        assert!(matches!(
            decision,
            LoginDecision::Refuse(LoginRefusal::UsernameTaken)
        ));
    }

    #[test]
    fn claimed_collision_refuses() {
        for rule in [MatchRule::UsernameOrEmail, MatchRule::VerifiedEmail] {
            let with_password = MUser {
                password: Some("$argon2id$...".into()),
                ..active()
            };
            let decision = decide(
                &identity(rule),
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
    }

    #[test]
    fn candidate_with_identity_refuses() {
        let bound = Some(Candidate {
            user: active(),
            has_identity: true,
        });
        let decision = decide(
            &identity(MatchRule::VerifiedEmail),
            None,
            None,
            bound,
            PasswordLogin::On,
        );
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
        let decision = decide(
            &identity(MatchRule::VerifiedEmail),
            None,
            None,
            unclaimed(with_password),
            PasswordLogin::Off,
        );
        assert!(matches!(decision, LoginDecision::Claim(_)));
    }

    #[test]
    fn oidc_keeps_password_accounts_unclaimable_when_password_login_off() {
        let with_password = MUser {
            password: Some("$argon2id$...".into()),
            ..active()
        };
        let decision = decide(
            &identity(MatchRule::UsernameOrEmail),
            None,
            unclaimed(with_password),
            None,
            PasswordLogin::Off,
        );
        assert!(matches!(
            decision,
            LoginDecision::Refuse(LoginRefusal::AccountExists)
        ));
    }

    #[test]
    fn verified_email_hints_on_username_clash_with_password_account() {
        let other = MUser {
            email: "someone-else@example.com".into(),
            password: Some("$argon2id$...".into()),
            ..active()
        };
        let decision = decide(
            &identity(MatchRule::VerifiedEmail),
            None,
            unclaimed(other),
            None,
            PasswordLogin::On,
        );
        assert!(matches!(
            decision,
            LoginDecision::Refuse(LoginRefusal::UsernameTaken)
        ));
    }

    #[test]
    fn no_match_creates() {
        let decision = decide(
            &identity(MatchRule::UsernameOrEmail),
            None,
            None,
            None,
            PasswordLogin::On,
        );
        assert!(matches!(decision, LoginDecision::Create));
    }
}
