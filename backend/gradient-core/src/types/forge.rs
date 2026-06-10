/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use num_enum::{IntoPrimitive, TryFromPrimitive};

/// Numeric encoding of `integration.forge_type`; the forge identity shared by
/// `gradient-forge` providers, `ci` integration lookups, and state export.
#[repr(i16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
pub enum ForgeType {
    Gitea = 0,
    Forgejo = 1,
    GitLab = 2,
    GitHub = 3,
}

impl ForgeType {
    pub fn from_path_segment(s: &str) -> Option<Self> {
        match s {
            "gitea" => Some(Self::Gitea),
            "forgejo" => Some(Self::Forgejo),
            "gitlab" => Some(Self::GitLab),
            "github" => Some(Self::GitHub),
            _ => None,
        }
    }

    /// Inverse of [`from_path_segment`](Self::from_path_segment): the canonical
    /// path/state segment naming this forge.
    pub const fn as_path_segment(self) -> &'static str {
        match self {
            Self::Gitea => "gitea",
            Self::Forgejo => "forgejo",
            Self::GitLab => "gitlab",
            Self::GitHub => "github",
        }
    }
}
