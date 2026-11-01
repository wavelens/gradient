/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::is_unique_violation;
use anyhow::{Context, Result};
use gradient_core::{ExternalIdentity, LoginMethods};
use gradient_db::WebDb;
use gradient_types::*;
use sea_orm::TransactionTrait;

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
    let tx = db
        .inner()
        .begin()
        .await
        .context("start connect transaction")?;

    let signed_in = gradient_db::lookup::by_id::<ESession, _>(&tx, session)
        .await
        .context("load connecting session")?
        .is_some_and(|s| s.user_id == user && s.is_usable_at(gradient_types::now()));
    if !signed_in {
        return Err(ConnectRefusal::SignedOut.into());
    }

    let active = gradient_db::lookup::by_id::<EUser, _>(&tx, user)
        .await
        .context("load connecting user")?
        .is_some_and(|u| u.active);
    if !active {
        return Err(ConnectRefusal::UserGone.into());
    }

    let linked =
        gradient_db::user_identities::user_by_identity(&tx, &identity.issuer, &identity.subject)
            .await
            .context("find linked user")?;
    match linked {
        Some(owner) if owner.id == user => return Ok(()),
        Some(_) => return Err(ConnectRefusal::TakenByOtherUser.into()),
        None => {}
    }

    let has_issuer = gradient_db::user_identities::of_user(&tx, user)
        .await
        .context("load identities")?
        .iter()
        .any(|i| i.issuer == identity.issuer);
    if has_issuer {
        return Err(ConnectRefusal::AlreadyConnected.into());
    }

    gradient_db::user_identities::insert(&tx, user, &identity.issuer, &identity.subject)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                ConnectRefusal::TakenByOtherUser.into()
            } else {
                anyhow::Error::new(e).context("insert identity")
            }
        })?;
    tx.commit().await.context("commit connect transaction")
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
}
