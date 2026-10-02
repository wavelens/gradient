/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_ci::IntegrationKind;
use gradient_types::actions::ActionConfig;
use gradient_types::triggers::{TriggerConfig, TriggerType};
use gradient_types::{
    GitHostType, MIntegration, MTask, MTaskAction, MTaskTrigger, TaskActionId, TaskTriggerId,
};
use sea_orm::{ActiveModelTrait, ConnectionTrait, IntoActiveModel};

#[derive(Debug, Default, PartialEq)]
pub(super) struct AutoAttach {
    pub inbound: Option<MIntegration>,
    pub outbound: Option<MIntegration>,
}

fn url_host(url: &str) -> Option<String> {
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

pub(super) fn match_integrations_for_repo(repo: &str, integrations: &[MIntegration]) -> AutoAttach {
    let Some(host) = url_host(repo) else {
        return AutoAttach::default();
    };
    let inferred = infer_git_host(&host, integrations);

    AutoAttach {
        inbound: pick_one(integrations, IntegrationKind::Inbound, &host, inferred),
        outbound: pick_one(integrations, IntegrationKind::Outbound, &host, inferred),
    }
}

pub(super) async fn apply<C: ConnectionTrait>(
    db: &C,
    task: &MTask,
    integrations: &[MIntegration],
) -> Result<(), sea_orm::DbErr> {
    let attach = match_integrations_for_repo(&task.repository, integrations);
    let now = gradient_types::now();

    if let Some(inbound) = attach.inbound {
        let cfg = TriggerConfig::ReporterPush {
            integration_id: inbound.id,
            branches: vec![],
            tags: vec![],
            releases_only: false,
        };
        MTaskTrigger {
            id: TaskTriggerId::now_v7(),
            task: task.id,
            trigger_type: TriggerType::ReporterPush,
            config: cfg.to_db_json(),
            active: true,
            created_at: now,
            updated_at: now,
            ..Default::default()
        }
        .into_active_model()
        .insert(db)
        .await?;
    }

    if let Some(outbound) = attach.outbound {
        let cfg = ActionConfig::GitHostStatusReport {
            integration_id: outbound.id,
        };
        MTaskAction {
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
        }
        .into_active_model()
        .insert(db)
        .await?;
    }

    Ok(())
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
