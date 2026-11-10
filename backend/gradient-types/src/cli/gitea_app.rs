/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::{Args, ValueEnum};
use gradient_derive::config_group;
use gradient_entity::integration::GitHostType;

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitHostSoftwareChoice {
    Gitea,
    Forgejo,
}

impl From<GitHostSoftwareChoice> for GitHostType {
    fn from(choice: GitHostSoftwareChoice) -> Self {
        match choice {
            GitHostSoftwareChoice::Gitea => Self::Gitea,
            GitHostSoftwareChoice::Forgejo => Self::Forgejo,
        }
    }
}

#[config_group("gitea_app")]
#[derive(Args, Debug, Clone)]
pub struct GiteaOAuth2ApplicationArgs {
    /// Base URL of the Gitea or Forgejo instance that has the OAuth2 application registered (example: `https://codeberg.org`).
    pub instance_base_url: Option<String>,
    /// Software to host Git on the instance.
    #[arg(value_enum, default_value_t = GitHostSoftwareChoice::Forgejo)]
    pub git_host_type: GitHostSoftwareChoice,
    /// The client ID for the OAuth2 application.
    pub client_id: Option<String>,
    /// Path to the file with the client secret for the OAuth2 application. Register an application on the Git host with redirect URI `<serve URL>/api/v1/integrations/oauth/callback`.
    pub client_secret_file: Option<String>,
}
