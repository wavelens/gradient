/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_ci::IntegrationKind;
use gradient_types::actions::{ActionConfig, ActionType};
use gradient_types::triggers::{TriggerConfig, TriggerType};
use gradient_types::{
    GitHostType, MIntegration, MTask, MTaskAction, MTaskTrigger, TaskActionId, TaskTriggerId,
};
use sea_orm::ConnectionTrait;

#[derive(Debug, Default, PartialEq)]
pub(crate) struct AutoAttach {
    pub inbound: Option<MIntegration>,
    pub outbound: Option<MIntegration>,
}

pub(crate) fn url_host(url: &str) -> Option<String> {
    let s = url.trim();
    let s = s.strip_prefix("git+").unwrap_or(s);

    for scheme in ["https://", "http://", "ssh://", "git://"] {
        if let Some(rest) = s.strip_prefix(scheme) {
            let rest = rest.rsplit('@').next().unwrap_or(rest);
            let host = rest.split(['/', ':']).next().unwrap_or("");
            return (!host.is_empty()).then(|| host.to_ascii_lowercase());
        }
    }

    let (prefix, _) = s.split_once(':')?;
    let host = prefix.rsplit('@').next().unwrap_or(prefix);
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

pub(crate) fn match_integrations_for_repo(repo: &str, integrations: &[MIntegration]) -> AutoAttach {
    let Some(host) = url_host(repo) else {
        return AutoAttach::default();
    };
    let inferred = infer_git_host(&host, integrations);

    AutoAttach {
        inbound: pick_one(integrations, IntegrationKind::Inbound, &host, inferred),
        outbound: pick_one(integrations, IntegrationKind::Outbound, &host, inferred),
    }
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Attached {
    pub push: bool,
    pub pull_request: bool,
    pub status: bool,
}

impl Attached {
    pub(crate) fn any(&self) -> bool {
        self.push || self.pull_request || self.status
    }
}

/// The function can add only the missing parts to the task. A rerun can happen after repository
/// URL changes or new integrations appear.
pub(crate) async fn apply<C: ConnectionTrait>(
    db: &C,
    task: &MTask,
    integrations: &[MIntegration],
) -> Result<Attached, sea_orm::DbErr> {
    let attach = match_integrations_for_repo(&task.repository, integrations);
    let now = gradient_types::now();
    let mut attached = Attached::default();

    let existing: Vec<TriggerType> = gradient_db::triggers::of_task(db, task.id)
        .await?
        .into_iter()
        .map(|t| t.trigger_type)
        .collect();
    if let Some(inbound) = attach.inbound {
        let wanted = [
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
                actions: ["opened", "synchronize", "reopened"]
                    .map(String::from)
                    .to_vec(),
                require_approval: true,
            },
        ];
        for cfg in wanted {
            if existing.contains(&cfg.trigger_type()) {
                continue;
            }
            match cfg.trigger_type() {
                TriggerType::ReporterPush => attached.push = true,
                _ => attached.pull_request = true,
            }
            let trigger = MTaskTrigger {
                id: TaskTriggerId::now_v7(),
                task: task.id,
                trigger_type: cfg.trigger_type(),
                config: cfg.to_db_json(),
                active: true,
                created_at: now,
                updated_at: now,
                ..Default::default()
            };
            gradient_db::triggers::insert(db, trigger).await?;
        }
    }

    let has_status_action = gradient_db::actions::of_task(db, task.id)
        .await?
        .iter()
        .any(|a| a.action_type == ActionType::GitHostStatusReport);
    if let Some(outbound) = attach.outbound.filter(|_| !has_status_action) {
        attached.status = true;
        let cfg = ActionConfig::GitHostStatusReport {
            integration_id: outbound.id,
        };
        let action = MTaskAction {
            id: TaskActionId::now_v7(),
            task: task.id,
            name: "Report status to Git host".into(),
            action_type: cfg.action_type(),
            config: serde_json::to_value(&cfg).unwrap_or_default(),
            events: serde_json::json!([]),
            active: true,
            created_by: task.created_by,
            created_at: now,
            updated_at: now,
            ..Default::default()
        };
        gradient_db::actions::insert(db, action).await?;
    }

    Ok(attached)
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
        assert_eq!(m, AutoAttach::default());
    }
}
