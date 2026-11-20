/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Serialize};

pub use gradient_entity::git_host_connection::GitHostType;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GitHostConnectionOwner {
    Instance,
    Team { name: String },
    Project { name: String },
}

impl std::str::FromStr for GitHostConnectionOwner {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once('/') {
            None if s == "instance" => Ok(Self::Instance),
            Some(("team", name)) if !name.is_empty() => Ok(Self::Team { name: name.into() }),
            Some(("project", name)) if !name.is_empty() => Ok(Self::Project { name: name.into() }),
            _ => Err(format!(
                "Set owner `{s}` to `instance`, `team/<name>`, or `project/<name>`."
            )),
        }
    }
}

impl std::fmt::Display for GitHostConnectionOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Instance => f.write_str("instance"),
            Self::Team { name } => write!(f, "team/{name}"),
            Self::Project { name } => write!(f, "project/{name}"),
        }
    }
}

pub struct GitHostConnectionCapabilities {
    pub github_app: bool,
    pub receives_events: bool,
    pub reports_status: bool,
    pub has_base_url: bool,
    pub has_access_token: bool,
    pub has_webhook_secret: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissingGitHostConnectionSetting {
    Capability,
    BaseUrlOrAccessTokenForStatus,
    WebhookSecretForEvents,
}

impl GitHostConnectionCapabilities {
    pub fn check(&self) -> Result<(), MissingGitHostConnectionSetting> {
        if !self.receives_events && !self.reports_status {
            return Err(MissingGitHostConnectionSetting::Capability);
        }
        if !self.github_app && self.reports_status && !(self.has_base_url && self.has_access_token)
        {
            return Err(MissingGitHostConnectionSetting::BaseUrlOrAccessTokenForStatus);
        }
        if !self.github_app && self.receives_events && !self.has_webhook_secret {
            return Err(MissingGitHostConnectionSetting::WebhookSecretForEvents);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_github_app_installation_needs_no_credentials() {
        let installation = GitHostConnectionCapabilities {
            github_app: true,
            receives_events: true,
            reports_status: true,
            has_base_url: false,
            has_access_token: false,
            has_webhook_secret: false,
        };
        assert_eq!(installation.check(), Ok(()));
        let token_without_url = GitHostConnectionCapabilities {
            github_app: false,
            receives_events: false,
            has_access_token: true,
            ..installation
        };
        assert_eq!(
            token_without_url.check(),
            Err(MissingGitHostConnectionSetting::BaseUrlOrAccessTokenForStatus)
        );
    }

    #[test]
    fn an_owner_reads_back_from_its_text_form() {
        for owner in [
            GitHostConnectionOwner::Instance,
            GitHostConnectionOwner::Team {
                name: "aliceteam".into(),
            },
            GitHostConnectionOwner::Project {
                name: "infra".into(),
            },
        ] {
            assert_eq!(
                owner.to_string().parse::<GitHostConnectionOwner>(),
                Ok(owner)
            );
        }
        assert!("team/".parse::<GitHostConnectionOwner>().is_err());
        assert!("cache/x".parse::<GitHostConnectionOwner>().is_err());
    }
}
