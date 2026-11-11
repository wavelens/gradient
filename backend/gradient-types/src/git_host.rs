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

#[cfg(test)]
mod tests {
    use super::GitHostConnectionOwner;

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
