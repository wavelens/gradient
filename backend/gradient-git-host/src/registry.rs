/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Resolved-once map of [`GitHostType`] -> [`GitHostProvider`], shared via
//! the `ci` context and the composed app state.

use std::collections::HashMap;
use std::sync::Arc;

use crate::provider::GitHostProvider;
use crate::providers::{gitea::GiteaProvider, github::GithubProvider, gitlab::GitlabProvider};
use gradient_types::GitHostType;

#[derive(Clone, Debug)]
pub struct GitHostRegistry {
    providers: Arc<HashMap<GitHostType, Arc<dyn GitHostProvider>>>,
}

impl GitHostRegistry {
    /// Registry of every Git host Gradient comes with. Adding a Git host is one
    /// `insert` here plus its `providers/*` impl.
    pub fn with_builtin() -> Self {
        let mut providers: HashMap<GitHostType, Arc<dyn GitHostProvider>> = HashMap::new();
        providers.insert(
            GitHostType::Gitea,
            Arc::new(GiteaProvider::new(GitHostType::Gitea)),
        );
        providers.insert(
            GitHostType::Forgejo,
            Arc::new(GiteaProvider::new(GitHostType::Forgejo)),
        );
        providers.insert(GitHostType::GitLab, Arc::new(GitlabProvider));
        providers.insert(GitHostType::GitHub, Arc::new(GithubProvider));

        Self {
            providers: Arc::new(providers),
        }
    }

    pub fn get(&self, git_host: GitHostType) -> Option<&Arc<dyn GitHostProvider>> {
        self.providers.get(&git_host)
    }
}

impl Default for GitHostRegistry {
    fn default() -> Self {
        Self::with_builtin()
    }
}
