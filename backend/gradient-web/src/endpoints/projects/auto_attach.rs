/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Best-effort wiring of a freshly created project to the org's forge
//! integrations. When the repository URL unambiguously belongs to one inbound
//! and/or one outbound integration, a push trigger and a status-report action
//! are attached so the project works with the forge without extra setup.

use gradient_ci::IntegrationKind;
use gradient_types::actions::ActionConfig;
use gradient_types::triggers::{TriggerConfig, TriggerType};
use gradient_types::{
    ForgeType, MIntegration, MProject, MProjectAction, MProjectTrigger, ProjectActionId,
    ProjectTriggerId,
};
use sea_orm::{ActiveModelTrait, ConnectionTrait, IntoActiveModel};

#[derive(Debug, Default, PartialEq)]
pub(super) struct AutoAttach {
    pub inbound: Option<MIntegration>,
    pub outbound: Option<MIntegration>,
}

/// Lowercased host of a git repository or forge endpoint URL. Handles
/// `https://`, `http://`, `ssh://`, `git://`, a `git+<scheme>` prefix and the
/// SCP form `git@host:owner/repo`.
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

fn public_forge_for_host(host: &str) -> Option<ForgeType> {
    match host {
        "github.com" => Some(ForgeType::GitHub),
        "gitlab.com" => Some(ForgeType::GitLab),
        _ => None,
    }
}

/// Forge identity behind `host`: a well-known public host, otherwise the forge
/// type of any integration whose endpoint URL points at the same host.
fn infer_forge(host: &str, integrations: &[MIntegration]) -> Option<ForgeType> {
    public_forge_for_host(host).or_else(|| {
        integrations.iter().find_map(|i| {
            let endpoint = i.endpoint_url.as_deref()?;
            (url_host(endpoint).as_deref() == Some(host)).then_some(i.forge_type)
        })
    })
}

/// An integration with a custom endpoint matches strictly by host; one without
/// (public forges, inbound rows) matches by the inferred forge type.
fn integration_matches(i: &MIntegration, host: &str, inferred: Option<ForgeType>) -> bool {
    if let Some(endpoint) = &i.endpoint_url {
        return url_host(endpoint).as_deref() == Some(host);
    }

    inferred.is_some_and(|f| f == i.forge_type)
}

/// The single integration of `kind` matching the repo, or `None` when zero or
/// more than one match (ambiguous wiring is left to the user).
fn pick_one(
    integrations: &[MIntegration],
    kind: IntegrationKind,
    host: &str,
    inferred: Option<ForgeType>,
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
    let inferred = infer_forge(&host, integrations);

    AutoAttach {
        inbound: pick_one(integrations, IntegrationKind::Inbound, &host, inferred),
        outbound: pick_one(integrations, IntegrationKind::Outbound, &host, inferred),
    }
}

/// Insert the trigger/action wiring for the matched integrations. Best-effort:
/// callers log and swallow errors so a wiring hiccup never blocks creation.
pub(super) async fn apply<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
    integrations: &[MIntegration],
) -> Result<(), sea_orm::DbErr> {
    let attach = match_integrations_for_repo(&project.repository, integrations);
    let now = gradient_types::now();

    if let Some(inbound) = attach.inbound {
        let cfg = TriggerConfig::ReporterPush {
            integration_id: inbound.id,
            branches: vec![],
            tags: vec![],
            releases_only: false,
        };
        MProjectTrigger {
            id: ProjectTriggerId::now_v7(),
            project: project.id,
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
        let cfg = ActionConfig::ForgeStatusReport {
            integration_id: outbound.id,
        };
        MProjectAction {
            id: ProjectActionId::now_v7(),
            project: project.id,
            name: "Report status to forge".into(),
            action_type: cfg.action_type(),
            config: serde_json::to_value(&cfg).unwrap_or_default(),
            events: serde_json::json!([]),
            active: true,
            created_by: project.created_by,
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

    fn integ(kind: IntegrationKind, forge: ForgeType, endpoint: Option<&str>) -> MIntegration {
        MIntegration {
            kind,
            forge_type: forge,
            endpoint_url: endpoint.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn host_parsing_covers_url_shapes() {
        assert_eq!(url_host("https://github.com/foo/bar").as_deref(), Some("github.com"));
        assert_eq!(url_host("git@gitea.example.com:foo/bar.git").as_deref(), Some("gitea.example.com"));
        assert_eq!(url_host("ssh://git@gitlab.com/foo/bar").as_deref(), Some("gitlab.com"));
        assert_eq!(url_host("git+https://Gitea.Example.com/foo").as_deref(), Some("gitea.example.com"));
        assert_eq!(url_host("not-a-url"), None);
    }

    #[test]
    fn self_hosted_pairs_inbound_and_outbound() {
        let integrations = vec![
            integ(IntegrationKind::Inbound, ForgeType::Gitea, None),
            integ(IntegrationKind::Outbound, ForgeType::Gitea, Some("https://gitea.example.com")),
        ];
        let m = match_integrations_for_repo("git@gitea.example.com:foo/bar.git", &integrations);
        assert!(m.inbound.is_some(), "inbound matched via inferred forge");
        assert!(m.outbound.is_some(), "outbound matched via endpoint host");
    }

    #[test]
    fn public_github_matches_by_forge_type() {
        let integrations = vec![
            integ(IntegrationKind::Inbound, ForgeType::GitHub, None),
            integ(IntegrationKind::Outbound, ForgeType::GitHub, None),
        ];
        let m = match_integrations_for_repo("https://github.com/foo/bar", &integrations);
        assert!(m.inbound.is_some());
        assert!(m.outbound.is_some());
    }

    #[test]
    fn ambiguous_inbound_is_skipped() {
        let integrations = vec![
            integ(IntegrationKind::Inbound, ForgeType::GitHub, None),
            integ(IntegrationKind::Inbound, ForgeType::GitHub, None),
        ];
        let m = match_integrations_for_repo("https://github.com/foo/bar", &integrations);
        assert!(m.inbound.is_none(), "two inbound matches is ambiguous");
    }

    #[test]
    fn unrelated_forge_does_not_match() {
        let integrations = vec![integ(
            IntegrationKind::Outbound,
            ForgeType::Gitea,
            Some("https://other-gitea.example.com"),
        )];
        let m = match_integrations_for_repo("https://github.com/foo/bar", &integrations);
        assert_eq!(m, AutoAttach::default());
    }
}
