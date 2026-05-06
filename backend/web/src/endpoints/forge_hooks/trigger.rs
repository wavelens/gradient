/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Evaluation triggering from forge webhooks.

use super::response::{QueuedEvaluation, SkippedProject, WebhookTriggerOutcome};
use entity::project_trigger as ept;
use gradient_core::ci::{apply_trigger, ApplyInput, ApplyOutcome};
use gradient_core::types::triggers::{ConcurrencyPolicy, TriggerConfig, TriggerType};
use gradient_core::types::*;
use scheduler::Scheduler;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, DbBackend, EntityTrait, IntoActiveModel, QueryFilter, Statement, Value};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{info, warn};

// ── GitHub installation payload ────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct GitHubInstallationPayload {
    pub action: String,
    pub installation: GitHubInstallation,
    pub sender: Option<GitHubSender>,
}

#[derive(Deserialize)]
pub(super) struct GitHubInstallation {
    pub id: i64,
    pub account: GitHubAccount,
}

#[derive(Deserialize)]
pub(super) struct GitHubAccount {
    pub login: String,
}

#[derive(Deserialize)]
pub(super) struct GitHubSender {
    pub login: String,
}

// ── GitHub App installation event ──────────────────────────────────────────

pub(super) async fn handle_github_installation(state: &Arc<ServerState>, body: &[u8]) {
    let payload: GitHubInstallationPayload = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "Failed to parse GitHub installation payload");
            return;
        }
    };

    if payload.action == "deleted" {
        clear_installation_id(state, payload.installation.id).await;
        return;
    }

    store_installation_id(state, &payload).await;
}

async fn clear_installation_id(state: &Arc<ServerState>, installation_id: i64) {
    if let Ok(orgs) = EOrganization::find()
        .filter(COrganization::GithubInstallationId.eq(installation_id))
        .all(&state.web_db)
        .await
    {
        for org in orgs {
            let mut active = org.into_active_model();
            active.github_installation_id = Set(None);
            if let Err(e) = active.update(&state.web_db).await {
                warn!(error = %e, "Failed to clear github_installation_id");
            }
        }
    }
}

async fn store_installation_id(state: &Arc<ServerState>, payload: &GitHubInstallationPayload) {
    let github_login = &payload.installation.account.login;
    if let Ok(Some(org)) = EOrganization::find()
        .filter(COrganization::Name.eq(github_login.as_str()))
        .one(&state.web_db)
        .await
    {
        let installation_id = payload.installation.id;
        let mut active = org.into_active_model();
        active.github_installation_id = Set(Some(installation_id));
        if let Err(e) = active.update(&state.web_db).await {
            warn!(error = %e, installation_id, org_name = %github_login, "Failed to store github_installation_id");
        } else {
            info!(installation_id, org_name = %github_login, "GitHub App installed on organization");
        }
    } else {
        let sender_login = payload
            .sender
            .as_ref()
            .map(|s| s.login.as_str())
            .unwrap_or("unknown");
        warn!(
            github_login = %github_login,
            sender = %sender_login,
            installation_id = payload.installation.id,
            "GitHub App installed but no matching Gradient organization found"
        );
    }
}

// ── GitHub App: resolve installation → integration ─────────────────────────

/// Look up the inbound GitHub integration for a GitHub App `installation_id`.
///
/// Returns `None` when no org owns the given installation or the org has no
/// inbound GitHub integration configured.
pub(super) async fn resolve_github_integration_id(
    state: &Arc<ServerState>,
    installation_id: i64,
) -> Option<IntegrationId> {
    use gradient_core::ci::IntegrationKind;

    let org = EOrganization::find()
        .filter(COrganization::GithubInstallationId.eq(installation_id))
        .one(&state.web_db)
        .await
        .ok()
        .flatten()?;

    EIntegration::find()
        .filter(CIntegration::Organization.eq(org.id))
        .filter(CIntegration::Kind.eq(IntegrationKind::Inbound.as_i16()))
        .filter(CIntegration::ForgeType.eq(gradient_core::ci::ForgeType::GitHub.as_i16()))
        .one(&state.web_db)
        .await
        .ok()
        .flatten()
        .map(|i| i.id)
}

// ── Ref kind ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub(super) enum PushRefKind<'a> {
    Branch(&'a str),
    Tag(&'a str),
}

// ── Fan-out functions ───────────────────────────────────────────────────────

pub(super) async fn trigger_push_for_integration(
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    integration_id: IntegrationId,
    ref_kind: PushRefKind<'_>,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
) -> WebhookTriggerOutcome {
    fan_out_triggers(
        state,
        scheduler,
        integration_id,
        TriggerType::ReporterPush,
        commit_hash,
        commit_message,
        author_name,
        |cfg| match cfg {
            TriggerConfig::ReporterPush { branches, tags, releases_only, .. } => {
                if *releases_only {
                    return FilterResult::Skip;
                }
                let matches = match ref_kind {
                    PushRefKind::Branch(name) => glob_matches(branches, name),
                    PushRefKind::Tag(name) => glob_matches(tags, name),
                };
                if matches { FilterResult::Fire } else { FilterResult::SkipFilter }
            }
            _ => FilterResult::Skip,
        },
    )
    .await
}

pub(super) async fn trigger_pr_for_integration(
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    integration_id: IntegrationId,
    branch: Option<&str>,
    action: &str,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
) -> WebhookTriggerOutcome {
    fan_out_triggers(
        state,
        scheduler,
        integration_id,
        TriggerType::ReporterPullRequest,
        commit_hash,
        commit_message,
        author_name,
        |cfg| match cfg {
            TriggerConfig::ReporterPullRequest { branches, actions, .. } => {
                if !actions.iter().any(|a| a == action) {
                    return FilterResult::SkipFilter;
                }
                let matches = match branch {
                    Some(b) => glob_matches(branches, b),
                    None => branches.is_empty(),
                };
                if matches { FilterResult::Fire } else { FilterResult::SkipFilter }
            }
            _ => FilterResult::Skip,
        },
    )
    .await
}

pub(super) async fn trigger_release_for_integration(
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    integration_id: IntegrationId,
    tag: Option<&str>,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
) -> WebhookTriggerOutcome {
    fan_out_triggers(
        state,
        scheduler,
        integration_id,
        TriggerType::ReporterPush,
        commit_hash,
        commit_message,
        author_name,
        |cfg| match cfg {
            TriggerConfig::ReporterPush { tags, releases_only, .. } => {
                if !releases_only {
                    return FilterResult::Skip;
                }
                let matches = match tag {
                    Some(t) => glob_matches(tags, t),
                    None => tags.is_empty(),
                };
                if matches { FilterResult::Fire } else { FilterResult::SkipFilter }
            }
            _ => FilterResult::Skip,
        },
    )
    .await
}

// ── Generic fan-out engine ─────────────────────────────────────────────────

enum FilterResult {
    /// Proceed to fire `apply_trigger`.
    Fire,
    /// Config filter did not match — add to skipped with reason "filter".
    SkipFilter,
    /// This trigger type / config shape doesn't apply at all — silently ignore.
    Skip,
}

async fn fan_out_triggers<F>(
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    integration_id: IntegrationId,
    trigger_type: TriggerType,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
    filter: F,
) -> WebhookTriggerOutcome
where
    F: Fn(&TriggerConfig) -> FilterResult,
{
    let triggers =
        match load_active_triggers_for_integration(state, integration_id, trigger_type).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "load triggers for integration");
                return WebhookTriggerOutcome::default();
            }
        };

    let mut outcome = WebhookTriggerOutcome::default();
    for trig in triggers {
        let cfg = match TriggerConfig::parse_row(trig.trigger_type, &trig.config) {
            Ok(c) => c,
            Err(e) => {
                warn!(trigger_id = %trig.id, error = %e, "skip trigger with invalid config");
                continue;
            }
        };

        match filter(&cfg) {
            FilterResult::Skip => continue,
            FilterResult::SkipFilter => {
                let (project_name, organization) = project_identity(state, trig.project).await;
                outcome.skipped.push(SkippedProject {
                    project_id: trig.project,
                    project_name,
                    organization,
                    reason: "filter".into(),
                });
                continue;
            }
            FilterResult::Fire => {}
        }

        let project = match EProject::find_by_id(trig.project).one(&state.web_db).await {
            Ok(Some(p)) => p,
            Ok(None) => {
                warn!(trigger_id = %trig.id, project_id = %trig.project, "project not found for trigger");
                continue;
            }
            Err(e) => {
                warn!(error = %e, trigger_id = %trig.id, "DB error fetching project for trigger");
                continue;
            }
        };
        outcome.projects_scanned += 1;

        let concurrency =
            ConcurrencyPolicy::from_i16(trig.concurrency).unwrap_or(ConcurrencyPolicy::HardAbort);
        let org_name = org_name_for(state, project.organization).await.unwrap_or_default();

        match apply_trigger(
            &state.web_db,
            &project,
            ApplyInput {
                trigger_id: trig.id,
                trigger_type,
                concurrency,
                commit_hash: commit_hash.clone(),
                commit_message: commit_message.clone(),
                author_name: author_name.clone(),
                manual: false,
            },
        )
        .await
        {
            Ok(ApplyOutcome::Created { evaluation: eval, aborted_evaluation, aborted_builds }) => {
                if let Some(aborted_id) = aborted_evaluation {
                    scheduler.cancel_evaluation_jobs(aborted_id, &aborted_builds).await;
                }
                info!(
                    project_id = %project.id,
                    evaluation_id = %eval.id,
                    "forge webhook trigger fired"
                );
                outcome.queued.push(QueuedEvaluation {
                    project_id: project.id,
                    project_name: project.name.clone(),
                    organization: org_name,
                    evaluation_id: eval.id,
                });
            }
            Ok(ApplyOutcome::SkippedSameCommit) => {
                outcome.skipped.push(SkippedProject {
                    project_id: project.id,
                    project_name: project.name.clone(),
                    organization: org_name,
                    reason: "same_commit".into(),
                });
            }
            Ok(ApplyOutcome::SkippedConcurrency) => {
                outcome.skipped.push(SkippedProject {
                    project_id: project.id,
                    project_name: project.name.clone(),
                    organization: org_name,
                    reason: "concurrency".into(),
                });
            }
            Ok(ApplyOutcome::SkippedAllowReserved) => {
                outcome.skipped.push(SkippedProject {
                    project_id: project.id,
                    project_name: project.name.clone(),
                    organization: org_name,
                    reason: "allow_reserved".into(),
                });
            }
            Err(e) => {
                warn!(error = %e, project_id = %project.id, "apply_trigger failed in webhook fan-out");
                outcome.skipped.push(SkippedProject {
                    project_id: project.id,
                    project_name: project.name.clone(),
                    organization: org_name,
                    reason: "error".into(),
                });
            }
        }
    }
    outcome
}

// ── Helpers ────────────────────────────────────────────────────────────────

async fn load_active_triggers_for_integration(
    state: &Arc<ServerState>,
    integration_id: IntegrationId,
    trigger_type: TriggerType,
) -> Result<Vec<ept::Model>, sea_orm::DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        &format!(
            "SELECT * FROM project_trigger \
             WHERE active = true \
               AND trigger_type = {} \
               AND (config->>'integration_id')::uuid = $1",
            trigger_type.as_i16(),
        ),
        [Value::Uuid(Some(Box::new(integration_id.into_inner())))],
    );
    EProjectTrigger::find()
        .from_raw_sql(stmt)
        .all(&state.web_db)
        .await
}

/// Simple glob match: `*` matches any sequence of characters (including none).
/// An empty `globs` list means "match everything".
fn glob_matches(globs: &[String], name: &str) -> bool {
    if globs.is_empty() {
        return true;
    }
    globs.iter().any(|g| glob_match_pattern(g, name))
}

fn glob_match_pattern(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    glob_match_recursive(&p, &t, 0, 0)
}

fn glob_match_recursive(p: &[char], t: &[char], pi: usize, ti: usize) -> bool {
    if pi == p.len() {
        return ti == t.len();
    }
    if p[pi] == '*' {
        // Skip consecutive stars.
        let next_pi = {
            let mut i = pi + 1;
            while i < p.len() && p[i] == '*' {
                i += 1;
            }
            i
        };
        // '*' at end matches everything remaining.
        if next_pi == p.len() {
            return true;
        }
        // Try matching '*' against 0..n characters.
        for advance in 0..=(t.len() - ti) {
            if glob_match_recursive(p, t, next_pi, ti + advance) {
                return true;
            }
        }
        false
    } else {
        if ti == t.len() {
            return false;
        }
        if p[pi] == t[ti] {
            glob_match_recursive(p, t, pi + 1, ti + 1)
        } else {
            false
        }
    }
}

async fn project_identity(
    state: &Arc<ServerState>,
    project_id: ProjectId,
) -> (String, String) {
    match EProject::find_by_id(project_id).one(&state.web_db).await {
        Ok(Some(p)) => {
            let org = org_name_for(state, p.organization).await.unwrap_or_default();
            (p.name, org)
        }
        _ => (String::new(), String::new()),
    }
}

async fn org_name_for(
    state: &Arc<ServerState>,
    org_id: OrganizationId,
) -> Option<String> {
    EOrganization::find_by_id(org_id)
        .one(&state.web_db)
        .await
        .ok()
        .flatten()
        .map(|o| o.name)
}

#[cfg(test)]
mod tests {
    use super::super::WebhookTriggerOutcome;
    use super::{glob_match_pattern, glob_matches};

    #[test]
    fn trigger_outcome_default_is_empty() {
        let o = WebhookTriggerOutcome::default();
        assert_eq!(o.projects_scanned, 0);
        assert!(o.queued.is_empty());
        assert!(o.skipped.is_empty());
    }

    #[test]
    fn glob_empty_list_matches_all() {
        assert!(glob_matches(&[], "main"));
        assert!(glob_matches(&[], "anything"));
    }

    #[test]
    fn glob_exact_match() {
        let globs = vec!["main".to_string()];
        assert!(glob_matches(&globs, "main"));
        assert!(!glob_matches(&globs, "develop"));
    }

    #[test]
    fn glob_star_prefix() {
        assert!(glob_match_pattern("feature/*", "feature/my-branch"));
        assert!(!glob_match_pattern("feature/*", "bugfix/my-branch"));
    }

    #[test]
    fn glob_star_only() {
        assert!(glob_match_pattern("*", "main"));
        assert!(glob_match_pattern("*", ""));
    }

    #[test]
    fn glob_version_pattern() {
        assert!(glob_match_pattern("v*", "v1.2.3"));
        assert!(!glob_match_pattern("v*", "1.2.3"));
    }

    #[test]
    fn glob_multiple_patterns() {
        let globs = vec!["main".to_string(), "release/*".to_string()];
        assert!(glob_matches(&globs, "main"));
        assert!(glob_matches(&globs, "release/1.0"));
        assert!(!glob_matches(&globs, "develop"));
    }
}
