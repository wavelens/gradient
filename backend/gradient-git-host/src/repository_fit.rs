/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::reporter::parse_owner_repo;
use gradient_types::GitHostType;
pub use gradient_types::git_host::repository_host;
use gradient_types::git_host::{
    check_task_repository_url_host_matches_git_host_connection_host,
    host_name_of_git_host_type_and_base_url,
};

pub struct GitHostConnectionAddress<'a> {
    pub git_host_type: GitHostType,
    pub base_url: Option<&'a str>,
    pub github_app_account: Option<&'a str>,
}

impl GitHostConnectionAddress<'_> {
    pub fn expected_host(&self) -> Option<String> {
        host_name_of_git_host_type_and_base_url(self.git_host_type, self.base_url)
    }

    pub fn check_repository(&self, repository: &str) -> Result<(), String> {
        check_task_repository_url_host_matches_git_host_connection_host(
            repository,
            self.git_host_type,
            self.base_url,
        )?;
        let Some((owner, _)) = parse_owner_repo(repository) else {
            return Err(format!(
                "Can't read the owner and repository from {repository}"
            ));
        };
        if let Some(account) = self.github_app_account
            && !owner.eq_ignore_ascii_case(account)
        {
            return Err(format!(
                "The GitHub App installed on {account} does not have access to the repository of {owner}."
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::git_host::url_host;

    #[test]
    fn host_parsing_covers_url_shapes() {
        assert_eq!(
            url_host("https://github.com/foo/bar").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            url_host("git@gitea.example.com:foo/bar.git").as_deref(),
            Some("gitea.example.com")
        );
        assert_eq!(
            url_host("ssh://git@gitlab.com:2222/foo/bar").as_deref(),
            Some("gitlab.com")
        );
        assert_eq!(url_host("not a url"), None);
        assert_eq!(
            repository_host("git+github:acme/infra").as_deref(),
            Some("github.com")
        );
    }

    #[test]
    fn check_repository_rejects_gitea_connection_without_base_url() {
        let address = GitHostConnectionAddress {
            git_host_type: GitHostType::Gitea,
            base_url: None,
            github_app_account: None,
        };
        let reason = address
            .check_repository("https://git.example.com/acme/infra")
            .unwrap_err();
        assert!(reason.contains("has no base URL"), "{reason}");
    }
}
