/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Resolved-once map of [`ForgeType`] -> [`ForgeProvider`], shared via
//! [`CiContext`](crate::ci::CiContext) and [`AppState`](crate::AppState).

use std::collections::HashMap;
use std::sync::Arc;

use crate::ci::integration_lookup::ForgeType;
use crate::forge::provider::ForgeProvider;
use crate::forge::providers::{gitea::GiteaProvider, github::GithubProvider, gitlab::GitlabProvider};

#[derive(Clone, Debug)]
pub struct ForgeRegistry {
    providers: Arc<HashMap<ForgeType, Arc<dyn ForgeProvider>>>,
}

impl ForgeRegistry {
    /// Registry of every forge Gradient ships with. Adding a forge is one
    /// `insert` here plus its `providers/*` impl.
    pub fn with_builtin() -> Self {
        let mut providers: HashMap<ForgeType, Arc<dyn ForgeProvider>> = HashMap::new();
        providers.insert(ForgeType::Gitea, Arc::new(GiteaProvider::new(ForgeType::Gitea)));
        providers.insert(
            ForgeType::Forgejo,
            Arc::new(GiteaProvider::new(ForgeType::Forgejo)),
        );
        providers.insert(ForgeType::GitLab, Arc::new(GitlabProvider));
        providers.insert(ForgeType::GitHub, Arc::new(GithubProvider));

        Self {
            providers: Arc::new(providers),
        }
    }

    pub fn get(&self, forge: ForgeType) -> Option<&Arc<dyn ForgeProvider>> {
        self.providers.get(&forge)
    }
}

impl Default for ForgeRegistry {
    fn default() -> Self {
        Self::with_builtin()
    }
}
