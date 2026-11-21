/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::is_unique_violation;
use anyhow::{Context, Result};
use gradient_core::{ExternalIdentity, LoginMethods};
use gradient_db::WebDb;
use gradient_db::users::{
    InsertWithAuthorisingSessionError, VerifiedFactForSessionOrCredentialCreation,
    insert_credential_after_locking_user_row_and_rechecking_authorisation,
};
use gradient_types::*;
use sea_orm::DatabaseTransaction;

pub fn can_disconnect(
    methods: &LoginMethods,
    has_password: bool,
    identities: &[MUserIdentity],
    issuer: &str,
) -> bool {
    (methods.password_login() && has_password)
        || identities
            .iter()
            .any(|i| i.issuer != issuer && methods.signs_in_with(&i.issuer))
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectRefusal {
    #[error("this account is connected to another user")]
    TakenByOtherUser,
    #[error("an account of this provider is already connected")]
    AlreadyConnected,
    #[error("the user no longer exists or is deactivated")]
    UserGone,
    #[error("the session that started the connection has ended")]
    SignedOut,
}

impl ConnectRefusal {
    pub fn query_code(&self) -> &'static str {
        match self {
            Self::TakenByOtherUser => "taken",
            Self::AlreadyConnected => "already_connected",
            Self::UserGone => "failed",
            Self::SignedOut => "signed_out",
        }
    }
}

pub async fn connect_identity(
    db: &WebDb,
    user: UserId,
    session: SessionId,
    identity: &ExternalIdentity,
) -> Result<()> {
    let issuer = identity.issuer.clone();
    let subject = identity.subject.clone();
    insert_credential_after_locking_user_row_and_rechecking_authorisation(
        db.inner(),
        user,
        VerifiedFactForSessionOrCredentialCreation::ExistingSignInSession(session),
        move |tx| {
            Box::pin(
                insert_identity_link_for_active_user_with_unlinked_issuer_and_subject(
                    tx, user, issuer, subject,
                ),
            )
        },
    )
    .await
    .map_err(|e| match e {
        InsertWithAuthorisingSessionError::AuthorisationNoLongerHoldsUnderUserRowLock => {
            ConnectRefusal::SignedOut.into()
        }
        InsertWithAuthorisingSessionError::Database(e) => {
            anyhow::Error::new(e).context("lock user row and connect identity")
        }
        InsertWithAuthorisingSessionError::InsertClosureError(e) => e,
    })
}

async fn insert_identity_link_for_active_user_with_unlinked_issuer_and_subject(
    tx: &DatabaseTransaction,
    user: UserId,
    issuer: String,
    subject: String,
) -> Result<()> {
    let active = gradient_db::lookup::by_id::<EUser, _>(tx, user)
        .await
        .context("load connecting user")?
        .is_some_and(|u| u.active);
    if !active {
        return Err(ConnectRefusal::UserGone.into());
    }

    let linked = gradient_db::user_identities::user_by_identity(tx, &issuer, &subject)
        .await
        .context("find linked user")?;
    match linked {
        Some(owner) if owner.id == user => return Ok(()),
        Some(_) => return Err(ConnectRefusal::TakenByOtherUser.into()),
        None => {}
    }

    let has_issuer = gradient_db::user_identities::of_user(tx, user)
        .await
        .context("load identities")?
        .iter()
        .any(|i| i.issuer == issuer);
    if has_issuer {
        return Err(ConnectRefusal::AlreadyConnected.into());
    }

    gradient_db::user_identities::insert(tx, user, &issuer, &subject)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                ConnectRefusal::TakenByOtherUser.into()
            } else {
                anyhow::Error::new(e).context("insert identity")
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_core::{AuthorizeRequest, CodeExchange, LoginProvider};
    use std::sync::Arc;

    const PROVIDER_ISSUER: &str = "https://provider.example.com";
    const OIDC_ISSUER: &str = "https://idp.example.com";

    #[derive(Debug)]
    struct Provider {
        replaces_password_login: bool,
    }

    #[async_trait::async_trait]
    impl LoginProvider for Provider {
        fn id(&self) -> &str {
            "provider"
        }
        fn label(&self) -> &str {
            "Provider"
        }
        fn issuer(&self) -> &str {
            PROVIDER_ISSUER
        }
        fn replaces_password_login(&self) -> bool {
            self.replaces_password_login
        }
        fn authorize_url(&self, _: AuthorizeRequest<'_>) -> anyhow::Result<url::Url> {
            unimplemented!()
        }
        async fn identity(
            &self,
            _: &reqwest::Client,
            _: CodeExchange<'_>,
        ) -> anyhow::Result<ExternalIdentity> {
            unimplemented!()
        }
    }

    fn methods(oidc: bool, replaces_password_login: bool) -> LoginMethods {
        LoginMethods {
            oidc,
            oidc_required: false,
            providers: vec![Arc::new(Provider {
                replaces_password_login,
            })],
        }
    }

    fn identity(issuer: &str) -> MUserIdentity {
        MUserIdentity {
            issuer: issuer.into(),
            ..Default::default()
        }
    }

    #[test]
    fn password_counts_when_password_login_on() {
        let identities = [identity(PROVIDER_ISSUER)];
        assert!(can_disconnect(
            &methods(true, false),
            true,
            &identities,
            PROVIDER_ISSUER
        ));
    }

    #[test]
    fn password_ignored_when_a_provider_replaces_it() {
        let identities = [identity(PROVIDER_ISSUER)];
        assert!(!can_disconnect(
            &methods(true, true),
            true,
            &identities,
            PROVIDER_ISSUER
        ));
    }

    #[test]
    fn oidc_identity_counts_only_when_oidc_configured() {
        let identities = [identity(PROVIDER_ISSUER), identity(OIDC_ISSUER)];
        assert!(can_disconnect(
            &methods(true, false),
            false,
            &identities,
            PROVIDER_ISSUER
        ));
        assert!(!can_disconnect(
            &methods(false, false),
            false,
            &identities,
            PROVIDER_ISSUER
        ));
    }

    #[test]
    fn provider_only_user_cannot_disconnect() {
        let identities = [identity(PROVIDER_ISSUER)];
        assert!(!can_disconnect(
            &methods(true, false),
            false,
            &identities,
            PROVIDER_ISSUER
        ));
    }

    #[tokio::test]
    async fn connect_identity_with_deleted_session_waits_for_user_row_lock_then_is_refused_without_identity_link()
     {
        use gradient_fixtures::{database::TestDatabase, rows};
        use sea_orm::TransactionTrait;

        let test_db = TestDatabase::create().await;
        let db = test_db.connection();
        let owner = gradient_db::rows::insert::<EUser, _>(&db, rows::user())
            .await
            .unwrap();
        let session = rows::live_session(SessionId::now_v7());
        gradient_db::rows::insert::<ESession, _>(&db, session.clone())
            .await
            .unwrap();
        let claim = db.begin().await.unwrap();
        gradient_db::users::lock_user_row_for_update(&claim, owner.id)
            .await
            .unwrap();

        let web_db = WebDb::new(db.clone());
        let identity = ExternalIdentity {
            issuer: PROVIDER_ISSUER.into(),
            subject: "previous-holder".into(),
            username: "previous-holder".into(),
            email: None,
            name: None,
            groups: None,
            refresh_profile: false,
        };
        let connecting = connect_identity(&web_db, owner.id, session.id, &identity);
        tokio::pin!(connecting);
        let waiting =
            tokio::time::timeout(std::time::Duration::from_millis(500), &mut connecting).await;
        gradient_db::users::revoke_previous_account_holder_access(&claim, owner.id)
            .await
            .unwrap();
        claim.commit().await.unwrap();

        assert!(waiting.is_err());
        let refused = connecting.await.unwrap_err();
        assert!(matches!(
            refused.downcast_ref::<ConnectRefusal>(),
            Some(ConnectRefusal::SignedOut)
        ));
        assert!(test_db.rows::<EUserIdentity>().await.is_empty());
        test_db.finish().await;
    }
}
