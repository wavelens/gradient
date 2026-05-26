/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Evaluation triggering from forge webhooks.

use super::response::{QueuedEvaluation, SkippedProject, WebhookTriggerOutcome};
use entity::project_trigger as ept;
use gradient_core::ci::{
    APPROVAL_ACTION_ID, ApplyInput, ApplyOutcome, ApprovalInfo, ForgeType, apply_trigger,
    find_approval_gated_eval, unpark_approval, unpark_approval_with_wildcard,
};
use gradient_core::types::triggers::{TriggerConfig, TriggerType};
use gradient_core::types::wildcard::Wildcard;
use gradient_core::types::*;
use scheduler::Scheduler;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DbBackend, EntityTrait, FromQueryResult, IntoActiveModel,
    QueryFilter, Statement, Value,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{info, warn};

/// PR metadata extracted from the forge webhook payload, used by the approval
/// gate to decide whether to park the evaluation pending maintainer approval.
#[derive(Debug, Clone, Default)]
pub(super) struct PullRequestApprovalContext {
    pub pr_number: Option<u64>,
    pub pr_author: Option<String>,
    pub is_fork: Option<bool>,
}

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
        let org_id = org.id;
        let creator = org.created_by;
        let mut active = org.into_active_model();
        active.github_installation_id = Set(Some(installation_id));
        if let Err(e) = active.update(&state.web_db).await {
            warn!(error = %e, installation_id, org_name = %github_login, "Failed to store github_installation_id");
            return;
        }
        info!(installation_id, org_name = %github_login, "GitHub App installed on organization");
        if let Err(e) =
            gradient_core::ci::ensure_github_app_integrations(&state.web_db, org_id, creator).await
        {
            warn!(error = %e, %org_id, "Failed to materialise GitHub App integration rows");
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

// ── GitHub App: resolve installation + repository URL → integrations ───────

/// Canonical form for matching `project.repository` against the URLs reported
/// by a forge webhook. Strips a trailing `.git`, drops a single trailing slash,
/// and rewrites `git@host:owner/repo` SSH URLs to the `https://host/owner/repo`
/// shape so the two webhook variants and the user-stored project URL collapse
/// to one identity.
pub(super) fn normalize_repo_url(url: &str) -> String {
    let s = url.trim().trim_end_matches('/');
    let s = s.strip_suffix(".git").unwrap_or(s);
    if let Some(rest) = s.strip_prefix("git@")
        && let Some((host, path)) = rest.split_once(':')
    {
        return format!("https://{}/{}", host, path);
    }
    s.to_string()
}

/// Resolve a GitHub App webhook to the set of inbound GitHub integrations
/// whose org owns a project matching one of the webhook's `repository_urls`.
///
/// A single GitHub App installation can serve multiple Gradient orgs whenever
/// those orgs each track repositories hosted under the same GitHub account
/// (you can only install the App once per GitHub account, but each gradient
/// org gets its own `github_installation_id` pointing at it). Matching purely
/// on `installation_id` therefore returns one arbitrary org and silently
/// drops the others — adding the repo-URL gate is what makes multi-org
/// installations fire the correct subset.
///
/// Returns an empty vec when no org carries this installation, when no
/// matching project exists, or when an org's inbound GitHub integration row
/// is missing.
pub(super) async fn resolve_github_app_targets(
    state: &Arc<ServerState>,
    installation_id: i64,
    repository_urls: &[String],
) -> Vec<IntegrationId> {
    use gradient_core::ci::IntegrationKind;
    use std::collections::HashSet;

    let candidate_orgs = EOrganization::find()
        .filter(COrganization::GithubInstallationId.eq(installation_id))
        .all(&state.web_db)
        .await
        .unwrap_or_default();

    if candidate_orgs.is_empty() {
        return Vec::new();
    }

    let webhook_urls: HashSet<String> = repository_urls
        .iter()
        .map(|u| normalize_repo_url(u))
        .collect();

    let mut integrations = Vec::new();
    for org in candidate_orgs {
        let projects = match EProject::find()
            .filter(CProject::Organization.eq(org.id))
            .all(&state.web_db)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, org_id = %org.id, "resolve_github_app_targets: project lookup failed");
                continue;
            }
        };
        let has_match = projects
            .iter()
            .any(|p| webhook_urls.contains(&normalize_repo_url(&p.repository)));
        if !has_match {
            continue;
        }
        let integration = EIntegration::find()
            .filter(CIntegration::Organization.eq(org.id))
            .filter(CIntegration::Kind.eq(i16::from(IntegrationKind::Inbound)))
            .filter(CIntegration::ForgeType.eq(i16::from(gradient_core::ci::ForgeType::GitHub)))
            .one(&state.web_db)
            .await
            .ok()
            .flatten();
        match integration {
            Some(i) => integrations.push(i.id),
            None => warn!(
                org_id = %org.id,
                "resolve_github_app_targets: org has matching project but no inbound github integration row"
            ),
        }
    }
    integrations
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
            TriggerConfig::ReporterPush {
                branches,
                tags,
                releases_only,
                ..
            } => {
                if *releases_only {
                    return FilterResult::Skip;
                }
                let matches = match ref_kind {
                    PushRefKind::Branch(name) => glob_matches(branches, name),
                    PushRefKind::Tag(name) => glob_matches(tags, name),
                };
                if matches {
                    FilterResult::Fire
                } else {
                    FilterResult::SkipFilter
                }
            }
            _ => FilterResult::Skip,
        },
        None,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn trigger_pr_for_integration(
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    integration_id: IntegrationId,
    branch: Option<&str>,
    action: &str,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
    approval_ctx: PullRequestApprovalContext,
    head_repo_clone_url: Option<String>,
) -> WebhookTriggerOutcome {
    let action_owned = action.to_string();
    let branch_owned = branch.map(str::to_string);
    fan_out_triggers(
        state,
        scheduler,
        integration_id,
        TriggerType::ReporterPullRequest,
        commit_hash,
        commit_message,
        author_name,
        |cfg| match cfg {
            TriggerConfig::ReporterPullRequest {
                branches,
                actions,
                require_approval,
                ..
            } => {
                if !actions.iter().any(|a| a == &action_owned) {
                    return FilterResult::SkipFilter;
                }
                let matches = match branch_owned.as_deref() {
                    Some(b) => glob_matches(branches, b),
                    None => branches.is_empty(),
                };
                if !matches {
                    return FilterResult::SkipFilter;
                }
                FilterResult::FirePr {
                    require_approval: *require_approval,
                }
            }
            _ => FilterResult::Skip,
        },
        Some(approval_ctx),
        head_repo_clone_url,
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
            TriggerConfig::ReporterPush {
                tags,
                releases_only,
                ..
            } => {
                if !releases_only {
                    return FilterResult::Skip;
                }
                let matches = match tag {
                    Some(t) => glob_matches(tags, t),
                    None => tags.is_empty(),
                };
                if matches {
                    FilterResult::Fire
                } else {
                    FilterResult::SkipFilter
                }
            }
            _ => FilterResult::Skip,
        },
        None,
        None,
    )
    .await
}

// ── Generic fan-out engine ─────────────────────────────────────────────────

enum FilterResult {
    /// Proceed to fire `apply_trigger` (push / release / time / polling).
    Fire,
    /// PR trigger matched; whether the approval gate engages depends on the
    /// PR being from a fork and the contributor failing the writer-trust probe.
    FirePr { require_approval: bool },
    /// Config filter did not match - add to skipped with reason "filter".
    SkipFilter,
    /// This trigger type / config shape doesn't apply at all - silently ignore.
    Skip,
}

#[allow(clippy::too_many_arguments)]
async fn fan_out_triggers<F>(
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    integration_id: IntegrationId,
    trigger_type: TriggerType,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
    filter: F,
    approval_ctx: Option<PullRequestApprovalContext>,
    repository_override: Option<String>,
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

        let pr_require_approval = match filter(&cfg) {
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
            FilterResult::Fire => false,
            FilterResult::FirePr { require_approval } => require_approval,
        };

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

        let org_name = org_name_for(state, project.organization)
            .await
            .unwrap_or_default();

        let gate_approval = if pr_require_approval {
            decide_pr_gate(approval_ctx.as_ref()).await
        } else {
            None
        };

        let apply_result = apply_trigger(
            &state.web_db,
            &project,
            ApplyInput {
                trigger_id: trig.id,
                trigger_type,
                commit_hash: commit_hash.clone(),
                commit_message: commit_message.clone(),
                author_name: author_name.clone(),
                manual: false,
                gate_approval,
                repository_override: repository_override.clone(),
            },
        )
        .await;
        // Touch the trigger row's `last_fired_at` for any outcome (created /
        // skipped / errored). Without this the UI shows "Last fired: never"
        // for triggers that only fire via webhook, since the polling loop
        // (the only other touch site) doesn't visit reporter triggers.
        touch_trigger_last_fired(state, &trig).await;
        match apply_result {
            Ok(ApplyOutcome::Created {
                evaluation: eval,
                aborted_evaluation,
                aborted_builds,
            }) => {
                if let Some(aborted_id) = aborted_evaluation {
                    scheduler
                        .cancel_evaluation_jobs(aborted_id, &aborted_builds)
                        .await;
                }
                info!(
                    project_id = %project.id,
                    evaluation_id = %eval.id,
                    "forge webhook trigger fired"
                );
                gradient_core::ci::actions::dispatch_evaluation_created(state, &eval).await;
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

/// Resolves whether a PR webhook fire should be gated on maintainer approval.
///
/// Fail-closed: returns `Some(ApprovalInfo)` whenever the PR is (or might be) a
/// fork, deferring the trust decision to maintainers via the forge UI / `/ci
/// run` comment. Same-repo PRs (`is_fork == Some(false)`) bypass the gate.
async fn decide_pr_gate(ctx: Option<&PullRequestApprovalContext>) -> Option<ApprovalInfo> {
    let ctx = ctx?;
    if matches!(ctx.is_fork, Some(false)) {
        return None;
    }
    Some(ApprovalInfo {
        pr_number: ctx.pr_number.unwrap_or(0),
        pr_author: ctx.pr_author.clone().unwrap_or_default(),
    })
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Stamp `last_fired_at` on the trigger row so the project-triggers UI can
/// show when the webhook last considered it. Best-effort: a DB error here
/// must not derail the rest of the webhook fan-out.
async fn touch_trigger_last_fired(state: &Arc<ServerState>, trig: &ept::Model) {
    let now = gradient_core::types::now();
    let mut active: ept::ActiveModel = trig.clone().into();
    active.last_fired_at = Set(Some(now));
    active.updated_at = Set(now);
    if let Err(e) = active.update(&state.web_db).await {
        warn!(error = %e, trigger_id = %trig.id, "failed to stamp trigger last_fired_at");
    }
}

async fn load_active_triggers_for_integration(
    state: &Arc<ServerState>,
    integration_id: IntegrationId,
    trigger_type: TriggerType,
) -> Result<Vec<ept::Model>, sea_orm::DbErr> {
    // Match active triggers of the right type for any project in the
    // organisation that owns this integration. The historical
    // `config->>'integration_id' = $1` filter was fragile: the GitHub App
    // seed migration creates fresh integration rows, so any trigger created
    // before that migration carries a stale UUID and silently stops
    // matching its own org's webhooks. Org-level matching is safe because
    // each org has at most one inbound integration per forge_type, which is
    // already disambiguated by the webhook route.
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!(
            "SELECT pt.* FROM project_trigger pt \
             JOIN project p ON pt.project = p.id \
             JOIN integration i ON i.organization = p.organization \
             WHERE pt.active = true \
               AND pt.trigger_type = {} \
               AND i.id = $1",
            i16::from(trigger_type),
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

async fn project_identity(state: &Arc<ServerState>, project_id: ProjectId) -> (String, String) {
    match EProject::find_by_id(project_id).one(&state.web_db).await {
        Ok(Some(p)) => {
            let org = org_name_for(state, p.organization)
                .await
                .unwrap_or_default();
            (p.name, org)
        }
        _ => (String::new(), String::new()),
    }
}

async fn org_name_for(state: &Arc<ServerState>, org_id: OrganizationId) -> Option<String> {
    EOrganization::find_by_id(org_id)
        .one(&state.web_db)
        .await
        .ok()
        .flatten()
        .map(|o| o.name)
}

// ── Approval unpark: GitHub `check_run.requested_action` ───────────────────

#[derive(Deserialize)]
struct GithubCheckRunRequestedAction<'a> {
    action: &'a str,
    requested_action: Option<GithubRequestedAction<'a>>,
    check_run: GithubCheckRunRef<'a>,
    repository: GithubRepoFull<'a>,
    sender: Option<GithubSender<'a>>,
}

#[derive(Deserialize)]
struct GithubRequestedAction<'a> {
    identifier: &'a str,
}

#[derive(Deserialize)]
struct GithubCheckRunRef<'a> {
    id: i64,
    #[serde(rename = "pull_requests", default)]
    _pull_requests: Vec<serde_json::Value>,
    #[serde(default)]
    _name: Option<&'a str>,
}

#[derive(Deserialize)]
struct GithubRepoFull<'a> {
    full_name: Option<&'a str>,
}

#[derive(Deserialize)]
struct GithubSender<'a> {
    login: &'a str,
}

/// Handle a GitHub App `check_run.requested_action` event. Verifies the sender
/// is a repo writer via the project's reporter, then re-queues the
/// approval-gated evaluation matched by `check_run.id` and fires a fresh
/// pending check.
pub(super) async fn handle_github_check_run(
    state: &Arc<ServerState>,
    _scheduler: &Arc<Scheduler>,
    body: &[u8],
) {
    let payload: GithubCheckRunRequestedAction = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "GitHub check_run: failed to parse payload");
            return;
        }
    };
    if payload.action != "requested_action" {
        return;
    }
    let Some(action) = payload.requested_action else {
        return;
    };
    if action.identifier != APPROVAL_ACTION_ID {
        return;
    }
    let Some(sender) = payload.sender else {
        warn!("GitHub check_run.requested_action: missing sender");
        return;
    };
    let Some(full_name) = payload.repository.full_name else {
        warn!("GitHub check_run.requested_action: missing repository.full_name");
        return;
    };
    let Some((owner, repo)) = full_name.split_once('/') else {
        warn!(
            full_name,
            "GitHub check_run.requested_action: malformed repo full_name"
        );
        return;
    };

    let check_id = payload.check_run.id;
    let Some(eval) = find_eval_by_check_id(state, check_id).await else {
        warn!(
            check_run_id = check_id,
            "GitHub check_run.requested_action: no evaluation with matching repo_check_id"
        );
        return;
    };
    let Some(project_id) = eval.project else {
        return;
    };

    if !sender_is_trusted(state, project_id, owner, repo, sender.login).await {
        warn!(
            evaluation_id = %eval.id,
            sender = %sender.login,
            "Rejecting approval click - sender is not a repo writer"
        );
        return;
    }

    match unpark_approval(&state.web_db, eval.id).await {
        Ok(Some(unparked)) => {
            info!(
                evaluation_id = %eval.id,
                sender = %sender.login,
                "PR approval gate cleared via GitHub action"
            );
            dispatch_approval_granted(state, &unparked).await;
        }
        Ok(None) => {
            warn!(
                evaluation_id = %eval.id,
                "Approval click but evaluation no longer in Waiting+Approval state"
            );
        }
        Err(e) => warn!(error = %e, evaluation_id = %eval.id, "Failed to unpark approval gate"),
    }
}

/// Flip the `Awaiting Approval` check to Success once the gate is cleared.
async fn dispatch_approval_granted(state: &Arc<ServerState>, eval: &MEvaluation) {
    let Some(project_id) = eval.project else {
        return;
    };
    let payload = serde_json::json!({
        "evaluation_id": eval.id,
        "project_id": project_id,
        "status": "evaluation.approval_granted",
    });
    gradient_core::ci::actions::dispatch_evaluation_event(
        state,
        project_id,
        "evaluation.approval_granted",
        payload,
    )
    .await;

    // Make the Evaluation check appear immediately as Pending so the PR shows
    // the gradient pipeline is now in flight. Without this the user only sees
    // the Approval check turn green and nothing else until the eval worker
    // actually picks the row up - looks like the click did nothing.
    gradient_core::ci::actions::dispatch_evaluation_created(state, eval).await;
}

async fn find_eval_by_check_id(state: &Arc<ServerState>, check_id: i64) -> Option<MEvaluation> {
    // `evaluation.check_run_ids` is a JSON map keyed by check-context name
    // (Awaiting Approval / Evaluation / Build {ep}). Any of the stored ids
    // can match the clicked check, so we scan the map's values.
    use sea_orm::{DatabaseBackend, FromQueryResult, Statement};

    #[derive(FromQueryResult)]
    struct Row {
        id: uuid::Uuid,
    }

    let row = Row::find_by_statement(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        r#"SELECT id FROM evaluation
           WHERE check_run_ids IS NOT NULL
             AND EXISTS (
                 SELECT 1
                 FROM jsonb_each(check_run_ids) AS kv
                 WHERE (kv.value)::text::bigint = $1
             )
           LIMIT 1"#,
        [sea_orm::Value::BigInt(Some(check_id))],
    ))
    .one(&state.web_db)
    .await
    .ok()
    .flatten()?;

    EEvaluation::find_by_id(entity::ids::EvaluationId::new(row.id))
        .one(&state.web_db)
        .await
        .ok()
        .flatten()
}

/// Trust probe for the approval-unpark flows. Resolves the project's active
/// `ForgeStatusReport` action and asks the forge whether `sender` has write
/// permission on `owner/repo`. Fails closed if no such action is configured,
/// the reporter can't be built, or the forge probe errors.
async fn sender_is_trusted(
    state: &Arc<ServerState>,
    project_id: ProjectId,
    owner: &str,
    repo: &str,
    sender: &str,
) -> bool {
    let reporter = match gradient_core::ci::actions::reporter_for_project(state, project_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return false,
        Err(e) => {
            warn!(error = %e, %project_id, "resolving ForgeStatusReport action for trust probe");
            return false;
        }
    };
    match reporter.is_repo_writer(owner, repo, sender).await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, %project_id, "is_repo_writer probe failed");
            false
        }
    }
}

// ── Approval unpark: `/ci run` comment on Gitea/Forgejo/GitLab + GitHub ────

#[derive(Deserialize)]
struct CommentPayload {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    comment: Option<CommentBody>,
    #[serde(default)]
    issue: Option<CommentIssue>,
    #[serde(default)]
    pull_request: Option<CommentIssue>,
    #[serde(default)]
    sender: Option<CommentSender>,
    #[serde(default)]
    repository: Option<CommentRepo>,
    /// GitLab Note Hook fields.
    #[serde(default)]
    object_attributes: Option<GitlabNoteAttrs>,
    #[serde(default)]
    user: Option<CommentSender>,
    #[serde(default)]
    project: Option<GitlabNoteProject>,
    #[serde(default)]
    merge_request: Option<GitlabNoteMr>,
}

#[derive(Deserialize, Default)]
struct CommentBody {
    body: Option<String>,
}

#[derive(Deserialize, Default)]
struct CommentIssue {
    number: Option<u64>,
}

#[derive(Deserialize, Default)]
struct CommentSender {
    #[serde(default)]
    login: Option<String>,
    #[serde(default)]
    username: Option<String>,
}

#[derive(Deserialize, Default)]
struct CommentRepo {
    #[serde(default)]
    full_name: Option<String>,
}

#[derive(Deserialize, Default)]
struct GitlabNoteAttrs {
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    noteable_type: Option<String>,
}

#[derive(Deserialize, Default)]
struct GitlabNoteProject {
    #[serde(default)]
    path_with_namespace: Option<String>,
}

#[derive(Deserialize, Default)]
struct GitlabNoteMr {
    #[serde(default)]
    iid: Option<u64>,
}

/// Handle a `/ci run` comment on a PR. Re-queues the approval-gated
/// evaluation when the commenter passes the writer-trust probe.
///
/// `integration_id` is `Some` for per-integration webhook routes (Gitea /
/// Forgejo / GitLab) and `None` for the shared GitHub App route - for the
/// latter we resolve the integration from `installation.id` in the body.
pub(super) async fn handle_issue_comment(
    state: &Arc<ServerState>,
    _scheduler: &Arc<Scheduler>,
    forge: ForgeType,
    integration_id: Option<IntegrationId>,
    body: &[u8],
) {
    let payload: CommentPayload = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "comment webhook: failed to parse payload");
            return;
        }
    };

    let (comment_body, pr_number, sender_login, owner_repo) = match forge {
        ForgeType::GitLab => {
            let Some(attrs) = payload.object_attributes else {
                return;
            };
            if attrs.noteable_type.as_deref() != Some("MergeRequest") {
                return;
            }
            let comment_body = attrs.note.unwrap_or_default();
            let pr_number = payload.merge_request.and_then(|m| m.iid);
            let sender = payload.user.and_then(|u| u.username.or(u.login));
            let owner_repo = payload.project.and_then(|p| p.path_with_namespace);
            (comment_body, pr_number, sender, owner_repo)
        }
        _ => {
            // GitHub uses `action == "created"`; Gitea uses
            // `action == "created"` too.
            if payload.action.as_deref() != Some("created") {
                return;
            }
            let comment_body = payload.comment.and_then(|c| c.body).unwrap_or_default();
            let pr_number = payload
                .pull_request
                .or(payload.issue)
                .and_then(|i| i.number);
            let sender = payload.sender.and_then(|s| s.login.or(s.username));
            let owner_repo = payload.repository.and_then(|r| r.full_name);
            (comment_body, pr_number, sender, owner_repo)
        }
    };

    let cmd = match parse_ci_run_command(&comment_body) {
        Some(cmd) => cmd,
        None => return,
    };
    let Some(pr_number) = pr_number else {
        warn!("comment webhook: /ci run but no PR number");
        return;
    };
    let Some(sender) = sender_login else {
        warn!("comment webhook: /ci run but no sender");
        return;
    };
    let Some(owner_repo) = owner_repo else {
        warn!("comment webhook: /ci run but no repo path");
        return;
    };
    let Some((owner, repo)) = owner_repo.rsplit_once('/') else {
        warn!(owner_repo, "comment webhook: malformed repo path");
        return;
    };

    let integration_ids: Vec<IntegrationId> = match integration_id {
        Some(id) => vec![id],
        None => {
            let installation_id = match github_installation_id_from_comment_body(body) {
                Some(id) => id,
                None => {
                    warn!("comment webhook (github): no installation_id");
                    return;
                }
            };
            let repo_urls = vec![
                format!("https://github.com/{owner_repo}"),
                format!("https://github.com/{owner_repo}.git"),
                format!("git@github.com:{owner_repo}.git"),
            ];
            let targets = resolve_github_app_targets(state, installation_id, &repo_urls).await;
            if targets.is_empty() {
                warn!(installation_id, %owner_repo, "comment webhook (github): no integration matched");
                return;
            }
            targets
        }
    };

    let wildcard_override: Option<String> = match &cmd {
        CiRunCommand::Plain => None,
        CiRunCommand::WithWildcard(raw) => match raw.parse::<Wildcard>() {
            Ok(_) => Some(raw.clone()),
            Err(e) => {
                warn!(wildcard = %raw, error = %e, "/ci run wildcard rejected");
                post_wildcard_error_comment(
                    state,
                    &integration_ids,
                    owner,
                    repo,
                    pr_number,
                    raw,
                    &e,
                )
                .await;
                return;
            }
        },
    };

    let mut unparked_any = false;
    for integration_id in &integration_ids {
        let project_ids = match active_project_ids_for_integration(state, *integration_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "comment webhook: failed to load project list");
                continue;
            }
        };
        for project_id in project_ids {
            let Ok(Some(eval)) =
                find_approval_gated_eval(&state.web_db, project_id, pr_number).await
            else {
                continue;
            };
            if !sender_is_trusted(state, project_id, owner, repo, &sender).await {
                warn!(
                    project_id = %project_id,
                    pr_number,
                    %sender,
                    "Rejecting /ci run - sender is not a repo writer"
                );
                continue;
            }
            let unpark_result = match &wildcard_override {
                None => unpark_approval(&state.web_db, eval.id).await,
                Some(w) => unpark_approval_with_wildcard(&state.web_db, eval.id, w).await,
            };
            match unpark_result {
                Ok(Some(unparked)) => {
                    info!(
                        evaluation_id = %eval.id,
                        pr_number,
                        %sender,
                        wildcard_override = wildcard_override.as_deref(),
                        "PR approval gate cleared via /ci run comment"
                    );
                    dispatch_approval_granted(state, &unparked).await;
                    unparked_any = true;
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(error = %e, evaluation_id = %eval.id, "Failed to unpark approval gate")
                }
            }
        }
    }
    if !unparked_any {
        debug_no_match(pr_number);
    }
}

/// Posts a reply comment to the PR explaining a `/ci run <wildcard>`
/// parse failure. Walks every project owned by the matching
/// integrations and uses the first project with a usable
/// `ForgeStatusReport` action's reporter. Failures are logged and
/// swallowed - a comment-post failure must not crash the webhook
/// handler.
async fn post_wildcard_error_comment(
    state: &Arc<ServerState>,
    integration_ids: &[IntegrationId],
    owner: &str,
    repo: &str,
    pr_number: u64,
    raw_wildcard: &str,
    parse_error: &gradient_core::types::input::InputError,
) {
    let body = format!(
        "Could not parse wildcard `{}`: {}",
        raw_wildcard, parse_error
    );

    for integration_id in integration_ids {
        let project_ids = match active_project_ids_for_integration(state, *integration_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, %integration_id, "/ci run wildcard: failed to load projects for reply comment");
                continue;
            }
        };
        for project_id in project_ids {
            let reporter =
                match gradient_core::ci::actions::reporter_for_project(state, project_id).await {
                    Ok(Some(r)) => r,
                    Ok(None) => continue,
                    Err(e) => {
                        warn!(error = %e, %project_id, "/ci run wildcard: resolving reporter for reply comment");
                        continue;
                    }
                };
            match reporter.post_pr_comment(owner, repo, pr_number, &body).await {
                Ok(()) => return,
                Err(e) => {
                    warn!(error = %e, %project_id, "/ci run wildcard: reply comment post failed, trying next project");
                }
            }
        }
    }
}

fn debug_no_match(pr_number: u64) {
    tracing::debug!(
        pr_number,
        "/ci run comment had no matching parked evaluation"
    );
}

/// Outcome of parsing a `/ci run [wildcard]` comment.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CiRunCommand {
    /// Bare `/ci run` — unpark with the wildcard already on the eval row.
    Plain,
    /// `/ci run <wildcard>` — unpark and override the eval row's wildcard
    /// with this raw string. Not yet validated; the caller must pass it
    /// through `Wildcard::from_str` and reject (with a reply comment) on
    /// parse failure.
    WithWildcard(String),
}

/// Lifts `/ci run` (with or without a wildcard argument) from a comment
/// body. The command must appear on its own line (after trimming
/// whitespace). Blank lines and forge quote-reply lines (`> …`) are
/// skipped so a maintainer can quote the PR context above the command;
/// any other prose before or after the command disqualifies the comment.
pub(super) fn parse_ci_run_command(body: &str) -> Option<CiRunCommand> {
    const PREFIX: &str = "/ci run";

    let mut found: Option<CiRunCommand> = None;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('>') {
            continue;
        }
        if found.is_some() {
            return None;
        }
        if trimmed.len() < PREFIX.len() {
            return None;
        }
        let (prefix, rest) = trimmed.split_at(PREFIX.len());
        if !prefix.eq_ignore_ascii_case(PREFIX) {
            return None;
        }
        if rest.is_empty() {
            found = Some(CiRunCommand::Plain);
            continue;
        }
        if !rest.starts_with(|c: char| c.is_ascii_whitespace()) {
            return None;
        }
        let arg = rest.trim();
        if arg.is_empty() {
            found = Some(CiRunCommand::Plain);
            continue;
        }
        found = Some(CiRunCommand::WithWildcard(arg.to_string()));
    }
    found
}

fn github_installation_id_from_comment_body(body: &[u8]) -> Option<i64> {
    #[derive(Deserialize)]
    struct WithInstallation {
        installation: Option<InstallationId>,
    }
    #[derive(Deserialize)]
    struct InstallationId {
        id: i64,
    }
    serde_json::from_slice::<WithInstallation>(body)
        .ok()
        .and_then(|p| p.installation)
        .map(|i| i.id)
}

async fn active_project_ids_for_integration(
    state: &Arc<ServerState>,
    integration_id: IntegrationId,
) -> Result<Vec<ProjectId>, sea_orm::DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT DISTINCT project FROM project_trigger \
         WHERE active = true \
           AND trigger_type = $1 \
           AND (config->>'integration_id')::uuid = $2"
            .to_string(),
        [
            Value::SmallInt(Some(i16::from(TriggerType::ReporterPullRequest))),
            Value::Uuid(Some(Box::new(integration_id.into_inner()))),
        ],
    );

    #[derive(sea_orm::FromQueryResult)]
    struct ProjectRow {
        project: ProjectId,
    }
    ProjectRow::find_by_statement(stmt)
        .all(&state.web_db)
        .await
        .map(|rows| rows.into_iter().map(|r| r.project).collect())
}

#[cfg(test)]
mod tests {
    use super::super::WebhookTriggerOutcome;
    use super::{
        CiRunCommand, glob_match_pattern, glob_matches, normalize_repo_url, parse_ci_run_command,
    };

    #[test]
    fn normalize_strips_dot_git_suffix() {
        assert_eq!(
            normalize_repo_url("https://github.com/owner/repo.git"),
            "https://github.com/owner/repo"
        );
    }

    #[test]
    fn normalize_strips_trailing_slash() {
        assert_eq!(
            normalize_repo_url("https://github.com/owner/repo/"),
            "https://github.com/owner/repo"
        );
    }

    #[test]
    fn normalize_rewrites_ssh_to_https() {
        assert_eq!(
            normalize_repo_url("git@github.com:owner/repo.git"),
            "https://github.com/owner/repo"
        );
    }

    #[test]
    fn normalize_passes_through_canonical_form() {
        assert_eq!(
            normalize_repo_url("https://github.com/owner/repo"),
            "https://github.com/owner/repo"
        );
    }

    #[test]
    fn normalize_collapses_url_variants() {
        let canonical = normalize_repo_url("https://github.com/owner/repo");
        for url in [
            "https://github.com/owner/repo.git",
            "https://github.com/owner/repo/",
            "git@github.com:owner/repo.git",
            "  https://github.com/owner/repo  ",
        ] {
            assert_eq!(normalize_repo_url(url), canonical, "input was {url}");
        }
    }

    #[test]
    fn parse_ci_run_command_plain_returns_plain() {
        assert!(matches!(
            parse_ci_run_command("/ci run"),
            Some(CiRunCommand::Plain)
        ));
        assert!(matches!(
            parse_ci_run_command("   /ci run   "),
            Some(CiRunCommand::Plain)
        ));
        assert!(matches!(
            parse_ci_run_command("\n/ci run\n"),
            Some(CiRunCommand::Plain)
        ));
    }

    #[test]
    fn parse_ci_run_command_case_insensitive_prefix() {
        assert!(matches!(
            parse_ci_run_command("/CI Run"),
            Some(CiRunCommand::Plain)
        ));
        assert!(matches!(
            parse_ci_run_command("/Ci RUN packages.*.*"),
            Some(CiRunCommand::WithWildcard(ref w)) if w == "packages.*.*"
        ));
    }

    #[test]
    fn parse_ci_run_command_with_wildcard() {
        assert!(matches!(
            parse_ci_run_command("/ci run packages.*.*"),
            Some(CiRunCommand::WithWildcard(ref w)) if w == "packages.*.*"
        ));
    }

    #[test]
    fn parse_ci_run_command_with_complex_wildcard() {
        let body = "/ci run packages.*.foo,!packages.x86_64-linux.broken";
        let Some(CiRunCommand::WithWildcard(w)) = parse_ci_run_command(body) else {
            panic!("expected WithWildcard");
        };
        assert_eq!(w, "packages.*.foo,!packages.x86_64-linux.broken");
    }

    #[test]
    fn parse_ci_run_command_trims_trailing_whitespace_around_wildcard() {
        let body = "   /ci run   packages.*.*   ";
        let Some(CiRunCommand::WithWildcard(w)) = parse_ci_run_command(body) else {
            panic!("expected WithWildcard");
        };
        assert_eq!(w, "packages.*.*");
    }

    #[test]
    fn parse_ci_run_command_after_quote_reply() {
        let body =
            "> @maintainer asked us to retrigger\n> after rebasing main\n\n/ci run packages.*.*";
        let Some(CiRunCommand::WithWildcard(w)) = parse_ci_run_command(body) else {
            panic!("expected WithWildcard");
        };
        assert_eq!(w, "packages.*.*");
    }

    #[test]
    fn parse_ci_run_command_rejects_unrelated() {
        assert!(parse_ci_run_command("looks good").is_none());
        assert!(parse_ci_run_command("/ci runfoo").is_none());
        assert!(parse_ci_run_command("foo\n/ci run\nbar").is_none());
        assert!(
            parse_ci_run_command("quote-reply context\n\n/ci run").is_none(),
            "non-quote prose before /ci run must reject"
        );
    }

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
