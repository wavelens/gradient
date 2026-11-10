/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::has_permission;
use crate::authorization::ApiKeyContext;
use crate::endpoints::git_host_hooks::installation::github_full_name;
use crate::error::WebResult;
use crate::permissions::Permission;
use gradient_ci::IntegrationKind;
use gradient_db::WebDb;
use gradient_types::triggers::TriggerConfig;
use gradient_types::{GitHostType, GitHubAppConfig, MIntegration, MTask, ProjectId, UserId};

pub(crate) const STATUS_ACTION_NAME: &str = "Report status to Git host";

pub(crate) fn default_pull_request_trigger_actions() -> Vec<String> {
    ["opened", "synchronize", "reopened"]
        .map(String::from)
        .to_vec()
}

#[derive(Debug, Default, PartialEq)]
struct MatchedIntegrations {
    inbound: Option<MIntegration>,
    outbound: Option<MIntegration>,
}

pub(crate) fn url_host(url: &str) -> Option<String> {
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

fn public_git_host_for(host: &str) -> Option<GitHostType> {
    match host {
        "github.com" => Some(GitHostType::GitHub),
        "gitlab.com" => Some(GitHostType::GitLab),
        _ => None,
    }
}

fn infer_git_host(host: &str, integrations: &[MIntegration]) -> Option<GitHostType> {
    public_git_host_for(host).or_else(|| {
        integrations.iter().find_map(|i| {
            let endpoint = i.endpoint_url.as_deref()?;
            (url_host(endpoint).as_deref() == Some(host)).then_some(i.git_host_type)
        })
    })
}

pub(crate) fn integration_serves_repository(
    repo: &str,
    integration: &MIntegration,
    integrations: &[MIntegration],
) -> bool {
    url_host(repo).is_some_and(|host| {
        let inferred = infer_git_host(&host, integrations);
        integration_matches(integration, &host, inferred)
    })
}

fn integration_matches(i: &MIntegration, host: &str, inferred: Option<GitHostType>) -> bool {
    if let Some(endpoint) = &i.endpoint_url {
        return url_host(endpoint).as_deref() == Some(host);
    }

    inferred.is_some_and(|f| f == i.git_host_type)
}

/// Ambiguous wiring is left to the user.
fn pick_one(
    integrations: &[MIntegration],
    kind: IntegrationKind,
    host: &str,
    inferred: Option<GitHostType>,
) -> Option<MIntegration> {
    let mut matched = integrations
        .iter()
        .filter(|i| i.kind == kind && integration_matches(i, host, inferred));
    let first = matched.next()?;
    matched.next().is_none().then(|| first.clone())
}

fn match_integrations_for_repo(repo: &str, integrations: &[MIntegration]) -> MatchedIntegrations {
    let Some(host) = url_host(repo) else {
        return MatchedIntegrations::default();
    };
    let inferred = infer_git_host(&host, integrations);

    MatchedIntegrations {
        inbound: pick_one(integrations, IntegrationKind::Inbound, &host, inferred),
        outbound: pick_one(integrations, IntegrationKind::Outbound, &host, inferred),
    }
}

pub(crate) fn suggest_push_and_pull_request_triggers_for_repository_url(
    repo: &str,
    integrations: &[MIntegration],
) -> Vec<TriggerConfig> {
    let Some(inbound) = match_integrations_for_repo(repo, integrations).inbound else {
        return Vec::new();
    };
    vec![
        TriggerConfig::ReporterPush {
            integration_id: inbound.id,
            branches: vec![],
            tags: vec![],
            releases_only: false,
            include_flake_inputs: false,
        },
        TriggerConfig::ReporterPullRequest {
            integration_id: inbound.id,
            branches: vec![],
            actions: default_pull_request_trigger_actions(),
            require_approval: true,
        },
    ]
}

async fn bind_github_app(
    web_db: &WebDb,
    http: &reqwest::Client,
    github_app: Option<&GitHubAppConfig>,
    task: &MTask,
) -> anyhow::Result<()> {
    let Some(app) = github_app else {
        return Ok(());
    };
    let Some(full_name) = github_full_name(&task.repository) else {
        return Ok(());
    };
    let pem = tokio::fs::read_to_string(&app.private_key_file).await?;
    let Some((installation_id, account)) =
        gradient_git_host::github_app::get_repository_installation(
            http, app.app_id, &pem, &full_name,
        )
        .await?
    else {
        return Ok(());
    };

    gradient_ci::link_project_github_installation(
        web_db.inner(),
        task.project,
        installation_id,
        Some(&account),
        task.created_by,
    )
    .await?;
    Ok(())
}

pub(crate) async fn configured_github_app_if_caller_can_manage_integrations<'a>(
    web_db: &WebDb,
    github_app: Option<&'a GitHubAppConfig>,
    user: UserId,
    project: ProjectId,
    api_key: Option<&ApiKeyContext>,
) -> WebResult<Option<&'a GitHubAppConfig>> {
    let Some(app) = github_app else {
        return Ok(None);
    };
    let allowed = has_permission(
        web_db,
        user,
        project,
        Permission::ManageIntegrations,
        api_key,
    )
    .await?;
    Ok(allowed.then_some(app))
}

pub(crate) async fn link_project_to_github_app_installation_by_repository_host(
    web_db: &WebDb,
    http: &reqwest::Client,
    github_app: Option<&GitHubAppConfig>,
    task: &MTask,
) {
    if let Err(e) = bind_github_app(web_db, http, github_app, task).await {
        tracing::warn!(task = %task.id, error = %e, "Failed to link the repository to the GitHub App's installation");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn integ(kind: IntegrationKind, git_host: GitHostType, endpoint: Option<&str>) -> MIntegration {
        MIntegration {
            kind,
            git_host_type: git_host,
            endpoint_url: endpoint.map(str::to_string),
            ..Default::default()
        }
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
            url_host("ssh://git@gitlab.com/foo/bar").as_deref(),
            Some("gitlab.com")
        );
        assert_eq!(
            url_host("git+https://Gitea.Example.com/foo").as_deref(),
            Some("gitea.example.com")
        );
        assert_eq!(url_host("not-a-url"), None);
    }

    #[test]
    fn an_at_sign_in_the_path_does_not_change_the_host() {
        assert_eq!(
            url_host("https://evil.example/x@gitlab.com/y").as_deref(),
            Some("evil.example")
        );
        assert_eq!(
            url_host("https://user:pw@gitea.example.com:3000/a/b").as_deref(),
            Some("gitea.example.com")
        );
        assert_eq!(url_host("evil.example/x@gitlab.com:y/z"), None);
    }

    #[test]
    fn self_hosted_pairs_inbound_and_outbound() {
        let integrations = vec![
            integ(IntegrationKind::Inbound, GitHostType::Gitea, None),
            integ(
                IntegrationKind::Outbound,
                GitHostType::Gitea,
                Some("https://gitea.example.com"),
            ),
        ];
        let m = match_integrations_for_repo("git@gitea.example.com:foo/bar.git", &integrations);
        assert!(m.inbound.is_some(), "inbound matched via inferred Git host");
        assert!(m.outbound.is_some(), "outbound matched via endpoint host");
    }

    #[test]
    fn public_github_matches_by_git_host_type() {
        let integrations = vec![
            integ(IntegrationKind::Inbound, GitHostType::GitHub, None),
            integ(IntegrationKind::Outbound, GitHostType::GitHub, None),
        ];
        let m = match_integrations_for_repo("https://github.com/foo/bar", &integrations);
        assert!(m.inbound.is_some());
        assert!(m.outbound.is_some());
    }

    #[test]
    fn ambiguous_inbound_is_skipped() {
        let integrations = vec![
            integ(IntegrationKind::Inbound, GitHostType::GitHub, None),
            integ(IntegrationKind::Inbound, GitHostType::GitHub, None),
        ];
        let m = match_integrations_for_repo("https://github.com/foo/bar", &integrations);
        assert!(m.inbound.is_none(), "two inbound matches is ambiguous");
    }

    #[test]
    fn unrelated_git_host_does_not_match() {
        let integrations = vec![integ(
            IntegrationKind::Outbound,
            GitHostType::Gitea,
            Some("https://other-gitea.example.com"),
        )];
        let m = match_integrations_for_repo("https://github.com/foo/bar", &integrations);
        assert_eq!(m, MatchedIntegrations::default());
    }
}
