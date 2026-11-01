/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use async_trait::async_trait;
use url::Url;

pub struct ExternalIdentity {
    pub issuer: String,
    pub subject: String,
    pub username: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub groups: Option<Vec<String>>,
    pub match_rule: MatchRule,
    pub refresh_profile: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchRule {
    UsernameOrEmail,
    VerifiedEmail,
}

#[derive(Debug, thiserror::Error)]
pub enum LoginRefusal {
    #[error("account is deactivated")]
    Deactivated,
    #[error("An account already exists with this username or email.")]
    AccountExists,
    #[error("The username is taken. Sign in and connect the account from user settings instead.")]
    UsernameTaken,
    #[error("The account has no verified primary email.")]
    NoVerifiedEmail,
    #[error("The username of the account cannot be used as a Gradient username.")]
    UnusableUsername,
}

impl LoginRefusal {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Deactivated => "deactivated",
            Self::AccountExists => "account_exists",
            Self::UsernameTaken => "username_taken",
            Self::NoVerifiedEmail => "no_verified_email",
            Self::UnusableUsername => "unusable_username",
        }
    }
}

pub struct AuthorizeRequest<'a> {
    pub redirect_uri: &'a str,
    pub state: &'a str,
    pub pkce_challenge: &'a str,
}

pub struct CodeExchange<'a> {
    pub code: &'a str,
    pub redirect_uri: &'a str,
    pub pkce_verifier: &'a str,
}

/// An OAuth sign-in next to OIDC. Return a `LoginRefusal` inside the error to show its message to the user.
#[async_trait]
pub trait LoginProvider: Send + Sync + std::fmt::Debug {
    fn id(&self) -> &str;
    fn label(&self) -> &str;
    fn issuer(&self) -> &str;
    fn replaces_password_login(&self) -> bool;
    fn authorize_url(&self, request: AuthorizeRequest<'_>) -> anyhow::Result<Url>;
    async fn identity(
        &self,
        http: &reqwest::Client,
        exchange: CodeExchange<'_>,
    ) -> anyhow::Result<ExternalIdentity>;
}

#[derive(Clone, Debug, Default)]
pub struct LoginMethods {
    pub oidc: bool,
    pub oidc_required: bool,
    pub providers: Vec<Arc<dyn LoginProvider>>,
}

impl LoginMethods {
    pub fn password_login(&self) -> bool {
        !self.oidc_required && !self.providers.iter().any(|p| p.replaces_password_login())
    }

    pub fn provider(&self, id: &str) -> Option<&Arc<dyn LoginProvider>> {
        self.providers.iter().find(|p| p.id() == id)
    }

    pub fn signs_in_with(&self, issuer: &str) -> bool {
        self.oidc || self.providers.iter().any(|p| p.issuer() == issuer)
    }
}
