/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub use gradient_api_types::GitHostConnectionOwner;
pub use gradient_entity::git_host_connection::GitHostType;

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

pub fn host_name_of_git_host_type_and_base_url(
    git_host_type: GitHostType,
    base_url: Option<&str>,
) -> Option<String> {
    match (base_url, git_host_type) {
        (Some(base_url), _) => url_host(base_url),
        (None, GitHostType::GitHub) => Some("github.com".to_owned()),
        (None, GitHostType::GitLab) => Some("gitlab.com".to_owned()),
        (None, GitHostType::Gitea | GitHostType::Forgejo) => None,
    }
}

pub fn check_task_repository_url_host_matches_git_host_connection_host(
    repository: &str,
    git_host_type: GitHostType,
    base_url: Option<&str>,
) -> Result<(), String> {
    let host = repository_host(repository)
        .ok_or_else(|| format!("Can't read the host of {repository}"))?;
    let expected = host_name_of_git_host_type_and_base_url(git_host_type, base_url)
        .ok_or_else(|| "The connection has no base URL".to_owned())?;
    if expected != host {
        return Err(format!(
            "Repository host {host} and connection host {expected} differ"
        ));
    }
    Ok(())
}

pub fn base_url_of_git_host_type_and_base_url(
    git_host_type: GitHostType,
    base_url: Option<&str>,
) -> Option<&str> {
    match (base_url, git_host_type) {
        (Some(base_url), _) => Some(base_url),
        (None, GitHostType::GitLab) => Some("https://gitlab.com"),
        (None, GitHostType::GitHub | GitHostType::Gitea | GitHostType::Forgejo) => None,
    }
}

pub fn base_url_of_git_host_connection(connection: &crate::MGitHostConnection) -> Option<&str> {
    base_url_of_git_host_type_and_base_url(connection.git_host_type, connection.base_url.as_deref())
}

pub fn host_name_of_git_host_connection(connection: &crate::MGitHostConnection) -> Option<String> {
    host_name_of_git_host_type_and_base_url(
        connection.git_host_type,
        connection.base_url.as_deref(),
    )
}

pub fn check_task_repository_url_can_use_git_host_connection(
    repository: &str,
    connection: &crate::MGitHostConnection,
) -> Result<(), String> {
    check_task_repository_url_host_matches_git_host_connection_host(
        repository,
        connection.git_host_type,
        connection.base_url.as_deref(),
    )?;
    if crate::flake_url::parse_owner_repo(repository).is_none() {
        return Err(format!("Can't read owner and repository from {repository}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MGitHostConnection;

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

    fn connection(git_host_type: GitHostType, base_url: Option<&str>) -> MGitHostConnection {
        MGitHostConnection {
            git_host_type,
            base_url: base_url.map(str::to_owned),
            ..Default::default()
        }
    }

    #[test]
    fn a_repository_on_the_connection_host_fits() {
        let gitea = connection(GitHostType::Gitea, Some("https://git.example.com"));
        for repo in [
            "https://git.example.com/acme/infra",
            "git@git.example.com:acme/infra.git",
            "git+https://git.example.com/acme/infra?dir=nix",
        ] {
            assert_eq!(
                check_task_repository_url_can_use_git_host_connection(repo, &gitea),
                Ok(()),
                "{repo}"
            );
        }
        let github = connection(GitHostType::GitHub, None);
        assert_eq!(
            check_task_repository_url_can_use_git_host_connection("github:acme/infra", &github),
            Ok(())
        );
    }

    #[test]
    fn a_repository_on_another_host_is_refused_with_both_hosts() {
        let github = connection(GitHostType::GitHub, None);
        let reason = check_task_repository_url_can_use_git_host_connection(
            "https://gitlab.com/a/b",
            &github,
        )
        .unwrap_err();
        assert!(
            reason.contains("gitlab.com") && reason.contains("github.com"),
            "{reason}"
        );
    }

    #[test]
    fn a_connection_without_base_url_fits_only_repositories_on_its_service_default_host() {
        let legacy_gitlab = connection(GitHostType::GitLab, None);
        for repo in ["https://gitlab.com/acme/infra", "gitlab:acme/infra"] {
            assert_eq!(
                check_task_repository_url_can_use_git_host_connection(repo, &legacy_gitlab),
                Ok(()),
                "{repo}"
            );
        }
        assert!(
            check_task_repository_url_can_use_git_host_connection(
                "https://gitlab.example.com/acme/infra",
                &legacy_gitlab
            )
            .is_err()
        );
        let legacy_gitea = connection(GitHostType::Gitea, None);
        assert!(
            check_task_repository_url_can_use_git_host_connection(
                "https://gitea.example.com/acme/infra",
                &legacy_gitea
            )
            .is_err()
        );
    }

    #[test]
    fn a_repository_without_owner_and_name_is_refused() {
        let gitea = connection(GitHostType::Gitea, Some("https://git.example.com"));
        let reason = check_task_repository_url_can_use_git_host_connection(
            "https://git.example.com",
            &gitea,
        )
        .unwrap_err();
        assert!(reason.contains("owner and repository"), "{reason}");
        assert!(
            check_task_repository_url_can_use_git_host_connection("not a url", &gitea).is_err()
        );
    }

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
