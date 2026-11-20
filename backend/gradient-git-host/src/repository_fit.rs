/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::reporter::parse_owner_repo;
use gradient_types::GitHostType;

pub struct GitHostConnectionAddress<'a> {
    pub git_host_type: GitHostType,
    pub base_url: Option<&'a str>,
    pub github_app_account: Option<&'a str>,
}

impl GitHostConnectionAddress<'_> {
    pub fn expected_host(&self) -> Option<String> {
        match (self.base_url, self.git_host_type) {
            (Some(base_url), _) => url_host(base_url),
            (None, GitHostType::GitHub) => Some("github.com".to_owned()),
            (None, GitHostType::GitLab) => Some("gitlab.com".to_owned()),
            (None, GitHostType::Gitea | GitHostType::Forgejo) => None,
        }
    }

    pub fn check_repository(&self, repository: &str) -> Result<(), String> {
        let host = repository_host(repository)
            .ok_or_else(|| format!("Cannot read the host of {repository}"))?;
        if let Some(expected) = self.expected_host()
            && expected != host
        {
            return Err(format!(
                "Repository host {host} and connection host {expected} differ"
            ));
        }
        let Some((owner, _)) = parse_owner_repo(repository) else {
            return Err(format!("Can't read owner and repository from {repository}"));
        };
        if let Some(account) = self.github_app_account
            && !owner.eq_ignore_ascii_case(account)
        {
            return Err(format!(
                "The GitHub App installation on {account} has no access to repositories of {owner}"
            ));
        }
        Ok(())
    }
}

pub fn repository_host(repository: &str) -> Option<String> {
    let lower = repository.trim().to_ascii_lowercase();
    let lower = lower.strip_prefix("git+").unwrap_or(&lower);
    if lower.starts_with("github:") {
        Some("github.com".to_owned())
    } else if lower.starts_with("gitlab:") {
        Some("gitlab.com".to_owned())
    } else {
        url_host(repository)
    }
}

pub fn url_host(url: &str) -> Option<String> {
    let s = url.trim();
    let s = s.strip_prefix("git+").unwrap_or(s);

    let authority = match ["https://", "http://", "ssh://", "git://"]
        .iter()
        .find_map(|scheme| s.strip_prefix(scheme))
    {
        Some(rest) => rest.split(['/', '?', '#']).next().unwrap_or(""),
        None => s.split_once(':')?.0,
    };
    if authority.contains('/') {
        return None;
    }
    let host_and_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = host_and_port.split(':').next().unwrap_or("");
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
