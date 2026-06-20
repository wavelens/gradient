/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Evaluation triggering from forge webhooks.

use super::response::{QueuedEvaluation, SkippedProject, WebhookTriggerOutcome};
use gradient_entity::project_trigger as ept;
use gradient_ci::{
    APPROVAL_ACTION_ID, ApplyInput, ApplyOutcome, ApprovalInfo, apply_trigger,
    find_approval_gated_eval, parse_owner_repo, set_evaluation_source_comment, unpark_approval,
    unpark_approval_with_wildcard,
};
use gradient_types::triggers::{TriggerConfig, TriggerType};
use gradient_types::wildcard::Wildcard;
use gradient_types::*;
use gradient_core::ServerState;
use gradient_forge::ParsedPullRequestReviewEvent;
use gradient_scheduler::Scheduler;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DbBackend, EntityTrait, FromQueryResult, QueryFilter,
    Statement, Value,
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// PR metadata extracted from the forge webhook payload, used by the approval
/// gate to decide whether to park the evaluation pending maintainer approval.
#[derive(Debug, Clone, Default)]
pub(super) struct PullRequestApprovalContext {
    pub pr_number: Option<u64>,
    pub pr_author: Option<String>,
    pub is_fork: Option<bool>,
    /// Login of the actor who triggered this PR event. When this actor is a
    /// trusted repo writer the approval gate is bypassed, so a maintainer
    /// force-pushing onto a contributor's branch does not re-park the run.
    pub sender: Option<String>,
}

// ── GitHub installation payload ────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct GitHubInstallationPayload {
    pub action: String,
    pub installation: GitHubInstallation,
    pub sender: Option<GitHubSender>,
    #[serde(default)]
    pub repositories: Vec<GitHubRepoRef>,
    #[serde(default)]
    pub repositories_added: Vec<GitHubRepoRef>,
}

impl GitHubInstallationPayload {
    /// Repositories the installation grants access to, as lowercased
    /// `owner/repo` full names. Drawn from `installation` (`repositories`) and
    /// `installation_repositories` (`repositories_added`) payloads alike.
    fn installed_full_names(&self) -> std::collections::HashSet<String> {
        self.repositories
            .iter()
            .chain(self.repositories_added.iter())
            .map(|r| r.full_name.to_ascii_lowercase())
            .collect()
    }
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
pub(super) struct GitHubRepoRef {
    pub full_name: String,
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
    use gradient_entity::github_installation::{Column as Col, Entity as E};
    if let Err(e) = E::delete_many()
        .filter(Col::InstallationId.eq(installation_id))
        .exec(&state.web_db)
        .await
    {
        warn!(error = %e, installation_id, "Failed to delete github_installation rows");
    }
}

async fn store_installation_id(state: &Arc<ServerState>, payload: &GitHubInstallationPayload) {
    use std::collections::HashSet;

    let github_login = payload.installation.account.login.as_str();
    let installation_id = payload.installation.id;
    let installed = payload.installed_full_names();

    if installed.is_empty() {
        debug!(installation_id, github_login, "GitHub App install carried no repository list; nothing to bind");
        return;
    }

    // Bind to every org owning a project whose repository URL resolves to one of
    // the installed repos. Matching is purely on the parsed `owner/repo` of the
    // stored URL, so flake shorthand (`github:owner/repo`) and every clone-URL
    // form match; neither the org name nor the project name need correspond.
    let owner_org_ids: HashSet<OrganizationId> = EProject::find()
        .filter(CProject::Repository.contains("github"))
        .all(&state.web_db)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|p| github_full_name(&p.repository).is_some_and(|n| installed.contains(&n)))
        .map(|p| p.organization)
        .collect();

    if owner_org_ids.is_empty() {
        let sender_login = payload
            .sender
            .as_ref()
            .map(|s| s.login.as_str())
            .unwrap_or("unknown");
        warn!(
            github_login,
            sender = %sender_login,
            installation_id,
            "GitHub App installed but no Gradient project tracks an installed repository"
        );

        return;
    }

    let orgs = EOrganization::find()
        .filter(COrganization::Id.is_in(owner_org_ids))
        .all(&state.web_db)
        .await
        .unwrap_or_default();

    for org in orgs {
        let org_id = org.id;
        let creator = org.created_by;
        let inst = match gradient_ci::upsert_github_installation(
            &state.web_db,
            org_id,
            installation_id,
            Some(github_login),
            creator,
        )
        .await
        {
            Ok(id) => id,
            Err(e) => {
                warn!(error = %e, installation_id, %org_id, "Failed to upsert github_installation");
                continue;
            }
        };

        info!(installation_id, %org_id, github_login = %github_login, "GitHub App installed on organization");
        let name = gradient_ci::github_integration_name(Some(github_login), installation_id);
        if let Err(e) =
            gradient_ci::ensure_github_app_integrations(&state.web_db, org_id, inst, &name, "GitHub", creator).await
        {
            warn!(error = %e, %org_id, "Failed to materialise GitHub App integration rows");
        }
    }
}

/// Lowercased `owner/repo` for a github.com repository, or `None` for a
/// non-github URL. Recognizes https, SCP-style SSH, and the Nix flake shorthand
/// (`github:owner/repo`) so a project's stored URL can be matched against the
/// `full_name`s carried by a GitHub App installation payload.
fn github_full_name(repo_url: &str) -> Option<String> {
    let lower = repo_url.to_ascii_lowercase();
    let is_github =
        lower.contains("github.com") || lower.starts_with("github:") || lower.starts_with("git+github:");
    if !is_github {
        return None;
    }

    parse_owner_repo(repo_url).map(|(owner, repo)| format!("{owner}/{repo}").to_ascii_lowercase())
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
/// drops the others - adding the repo-URL gate is what makes multi-org
/// installations fire the correct subset.
///
/// Returns an empty vec when no org carries this installation, when no
/// matching project exists, or when an org's inbound GitHub integration row
/// is missing.
pub(super) async fn resolve_github_app_targets(
    state: &Arc<ServerState>,
    installation_id: i64,
    repository_urls: &[String],
    client_ip: std::net::IpAddr,
) -> Vec<IntegrationId> {
    use gradient_ci::IntegrationKind;
    use crate::ip_allowlist::is_allowed as ip_allowed;
    use std::collections::HashSet;

    use gradient_entity::github_installation::{Column as GiCol, Entity as EGi};

    let installs = EGi::find()
        .filter(GiCol::InstallationId.eq(installation_id))
        .all(&state.web_db)
        .await
        .unwrap_or_default();

    if installs.is_empty() {
        return Vec::new();
    }

    let webhook_urls: HashSet<String> = repository_urls
        .iter()
        .map(|u| normalize_repo_url(u))
        .collect();

    let mut integrations = Vec::new();
    for inst in installs {
        let org_id = inst.organization;
        let projects = match EProject::find()
            .filter(CProject::Organization.eq(org_id))
            .all(&state.web_db)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, %org_id, "resolve_github_app_targets: project lookup failed");
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
            .filter(CIntegration::Organization.eq(org_id))
            .filter(CIntegration::Kind.eq(i16::from(IntegrationKind::Inbound)))
            .filter(CIntegration::ForgeType.eq(i16::from(gradient_types::ForgeType::GitHub)))
            .filter(CIntegration::GithubInstallation.eq(inst.id))
            .one(&state.web_db)
            .await
            .ok()
            .flatten();
        match integration {
            Some(i) => {
                let allowlist = i.allowed_ips.clone().unwrap_or_default();
                if !ip_allowed(client_ip, &allowlist) {
                    warn!(
                        %org_id,
                        integration_id = %i.id,
                        %client_ip,
                        "resolve_github_app_targets: source IP not allowed, skipping integration"
                    );
                    continue;
                }
                integrations.push(i.id);
            }
            None => warn!(
                %org_id,
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
        false,
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
    manual: bool,
    wildcard_override: Option<String>,
    source_comment: Option<serde_json::Value>,
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
        manual,
        wildcard_override,
        source_comment,
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
        false,
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
    manual: bool,
    wildcard_override: Option<String>,
    source_comment: Option<serde_json::Value>,
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

    // Persist PR number/author on the evaluation (via `source_comment`) for every
    // PR trigger, so the UI can render "PR #42" with a link (#391). A
    // comment-triggered run already carries a richer source_comment (with a
    // `comment_id`), so only synthesize one when absent.
    let source_comment = source_comment.or_else(|| {
        approval_ctx.as_ref().and_then(|c| {
            c.pr_number
                .map(|n| serde_json::json!({ "pr_number": n, "pr_author": c.pr_author }))
        })
    });

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
            decide_pr_gate(state, &project, approval_ctx.as_ref()).await
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
                manual,
                gate_approval,
                repository_override: repository_override.clone(),
                wildcard_override: wildcard_override.clone(),
                source_comment: source_comment.clone(),
                instance_max_storage_gb: state.config.storage.max_storage_gb,
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
                aborted_anchors,
            }) => {
                if let Some(aborted_id) = aborted_evaluation {
                    scheduler
                        .cancel_evaluation_jobs(aborted_id, &aborted_anchors)
                        .await;
                }
                info!(
                    project_id = %project.id,
                    evaluation_id = %eval.id,
                    "forge webhook trigger fired"
                );
                gradient_ci::actions::dispatch_evaluation_created(&state.ci(), &eval).await;
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
/// fork, deferring the trust decision to maintainers via the forge UI /
/// `/gradient run` comment. The gate is bypassed when:
/// - the PR is same-repo (`is_fork == Some(false)`), or
/// - the actor who triggered the event (`sender`) is a trusted repo writer,
///   so a maintainer force-pushing onto a contributor's branch runs without
///   re-parking.
async fn decide_pr_gate(
    state: &Arc<ServerState>,
    project: &MProject,
    ctx: Option<&PullRequestApprovalContext>,
) -> Option<ApprovalInfo> {
    let ctx = ctx?;
    let sender_trusted = match ctx.sender.as_deref() {
        Some(sender) if !matches!(ctx.is_fork, Some(false)) => {
            match parse_owner_repo(&project.repository) {
                Some((owner, repo)) => {
                    sender_is_trusted(state, project.id, &owner, &repo, sender).await
                }
                None => false,
            }
        }
        _ => false,
    };
    gate_decision(ctx, sender_trusted)
}

/// Pure approval-gate decision given whether the event's actor is a trusted
/// repo writer. Returns `None` (run immediately) when the PR is same-repo or
/// the actor is trusted; otherwise `Some(ApprovalInfo)` to park for approval.
fn gate_decision(
    ctx: &PullRequestApprovalContext,
    sender_trusted: bool,
) -> Option<ApprovalInfo> {
    if matches!(ctx.is_fork, Some(false)) || sender_trusted {
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
    let now = gradient_types::now();
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
            if let Some(pr_number) = approval_pr_number(&eval) {
                submit_pr_approval_review(state, project_id, owner, repo, pr_number).await;
            }
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
    gradient_ci::actions::dispatch_evaluation_event(
        &state.ci(),
        project_id,
        "evaluation.approval_granted",
        payload,
    )
    .await;

    // Make the Evaluation check appear immediately as Pending so the PR shows
    // the gradient pipeline is now in flight. Without this the user only sees
    // the Approval check turn green and nothing else until the eval worker
    // actually picks the row up - looks like the click did nothing.
    gradient_ci::actions::dispatch_evaluation_created(&state.ci(), eval).await;
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

    EEvaluation::find_by_id(gradient_entity::ids::EvaluationId::new(row.id))
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
    let reporter = match gradient_ci::actions::reporter_for_project(&state.ci(), project_id).await {
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

// ── Approval unpark: native forge PR review (#369) ─────────────────────────

/// Find the approval-gated evaluation for `(project_id, pr_number)`, flip it
/// back to `Queued`, and re-emit its pending CI checks. Shared low-level step
/// of the comment, check-run-button, and native-review approval paths.
async fn unpark_pr_approval_eval(
    state: &Arc<ServerState>,
    project_id: ProjectId,
    pr_number: u64,
) -> Option<MEvaluation> {
    let eval = find_approval_gated_eval(&state.web_db, project_id, pr_number)
        .await
        .ok()
        .flatten()?;
    match unpark_approval(&state.web_db, eval.id).await {
        Ok(Some(unparked)) => {
            dispatch_approval_granted(state, &unparked).await;
            Some(unparked)
        }
        Ok(None) => None,
        Err(e) => {
            warn!(error = %e, evaluation_id = %eval.id, "Failed to unpark approval gate via review");
            None
        }
    }
}

/// PR number carried by an approval-gated evaluation's waiting reason.
fn approval_pr_number(eval: &MEvaluation) -> Option<u64> {
    match eval.waiting_reason.as_ref().and_then(WaitingReason::from_json)? {
        WaitingReason::Approval { pr_number, .. } => Some(pr_number),
        _ => None,
    }
}

/// Reflect a Gradient maintainer approval back onto the forge by submitting an
/// approving PR review (GitHub only; other forges no-op). Best-effort - a forge
/// hiccup never undoes the local unpark.
async fn submit_pr_approval_review(
    state: &Arc<ServerState>,
    project_id: ProjectId,
    owner: &str,
    repo: &str,
    pr_number: u64,
) {
    let reporter = match gradient_ci::actions::reporter_for_project(&state.ci(), project_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            warn!(error = %e, %project_id, "resolving reporter for PR approval review");
            return;
        }
    };

    if let Err(e) = reporter
        .approve_pull_request(owner, repo, pr_number, "Approved via Gradient maintainer approval gate.")
        .await
    {
        warn!(error = %e, %project_id, pr_number, "submitting forge PR approval review failed");
    }
}

/// Handle a `pull_request_review` webhook. A maintainer's native **approving**
/// review releases an approval-gated run for the PR, mirroring `/gradient
/// approve` and the GitHub "Approve and Run" check action (#369). Non-approving
/// reviews, and reviews by non-writers, are ignored. GitLab is a no-op: it
/// emits no webhook on merge-request approval.
pub(super) async fn handle_pull_request_review(
    state: &Arc<ServerState>,
    forge: ForgeType,
    integration_id: Option<IntegrationId>,
    body: &[u8],
    client_ip: std::net::IpAddr,
) {
    let parsed = match forge {
        ForgeType::GitHub => ParsedPullRequestReviewEvent::from_github(body),
        ForgeType::Gitea | ForgeType::Forgejo => ParsedPullRequestReviewEvent::from_gitea(body),
        ForgeType::GitLab => return,
    };
    let Some(review) = parsed else {
        return;
    };
    if !review.approved {
        return;
    }

    let Some(pr_number) = review.pr_number else {
        warn!("pull_request_review: approval without a PR number");
        return;
    };
    let Some(reviewer) = review.reviewer else {
        warn!(pr_number, "pull_request_review: approval without a reviewer");
        return;
    };
    let Some(owner_repo) = review.repository_full_name else {
        warn!(pr_number, "pull_request_review: approval without a repository");
        return;
    };
    let Some((owner, repo)) = owner_repo.rsplit_once('/') else {
        warn!(owner_repo, "pull_request_review: malformed repo full_name");
        return;
    };

    let integration_ids: Vec<IntegrationId> = match integration_id {
        Some(id) => vec![id],
        None => {
            let Some(installation_id) = github_installation_id_from_comment_body(body) else {
                warn!("pull_request_review (github): no installation_id");
                return;
            };
            let repo_urls = vec![
                format!("https://github.com/{owner_repo}"),
                format!("https://github.com/{owner_repo}.git"),
                format!("git@github.com:{owner_repo}.git"),
            ];
            let targets =
                resolve_github_app_targets(state, installation_id, &repo_urls, client_ip).await;
            if targets.is_empty() {
                warn!(installation_id, %owner_repo, "pull_request_review (github): no integration matched");
                return;
            }
            targets
        }
    };

    for integration_id in &integration_ids {
        let project_ids = match active_project_ids_for_integration(state, *integration_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "pull_request_review: failed to load project list");
                continue;
            }
        };
        let Some(probe_project) = first_project_with_reporter(state, &project_ids).await else {
            continue;
        };
        if !sender_is_trusted(state, probe_project, owner, repo, &reviewer).await {
            warn!(
                %integration_id,
                pr_number,
                %reviewer,
                "Ignoring PR review approval - reviewer is not a repo writer"
            );
            continue;
        }
        for project_id in &project_ids {
            if let Some(unparked) = unpark_pr_approval_eval(state, *project_id, pr_number).await {
                info!(
                    evaluation_id = %unparked.id,
                    pr_number,
                    %reviewer,
                    "PR approval gate cleared via native forge review"
                );
            }
        }
    }
}

// ── /gradient commands: PR-comment dispatch (Gitea/Forgejo/GitLab + GitHub) ─

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
    #[serde(default)]
    id: Option<i64>,
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
    id: Option<i64>,
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

/// Handle a `/gradient run [wildcard]` or `/gradient approve` comment on a PR.
/// Both commands are maintainer-only. `run` unparks an existing approval gate
/// if one exists for the PR, and otherwise creates a fresh evaluation; `approve`
/// only unparks.
///
/// `integration_id` is `Some` for per-integration webhook routes (Gitea /
/// Forgejo / GitLab) and `None` for the shared GitHub App route - for the
/// latter we resolve the integration from `installation.id` in the body.
pub(super) async fn handle_issue_comment(
    state: &Arc<ServerState>,
    scheduler: &Arc<Scheduler>,
    forge: ForgeType,
    integration_id: Option<IntegrationId>,
    body: &[u8],
    client_ip: std::net::IpAddr,
) {
    let payload: CommentPayload = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "comment webhook: failed to parse payload");
            return;
        }
    };

    let (comment_body, pr_number, sender_login, owner_repo, comment_id) = match forge {
        ForgeType::GitLab => {
            let Some(attrs) = payload.object_attributes else {
                return;
            };
            if attrs.noteable_type.as_deref() != Some("MergeRequest") {
                return;
            }
            let comment_id = attrs.id;
            let comment_body = attrs.note.unwrap_or_default();
            let pr_number = payload.merge_request.and_then(|m| m.iid);
            let sender = payload.user.and_then(|u| u.username.or(u.login));
            let owner_repo = payload.project.and_then(|p| p.path_with_namespace);
            (comment_body, pr_number, sender, owner_repo, comment_id)
        }
        _ => {
            // GitHub and Gitea both use `action == "created"`.
            if payload.action.as_deref() != Some("created") {
                return;
            }
            let comment = payload.comment.unwrap_or_default();
            let comment_id = comment.id;
            let comment_body = comment.body.unwrap_or_default();
            let pr_number = payload
                .pull_request
                .or(payload.issue)
                .and_then(|i| i.number);
            let sender = payload.sender.and_then(|s| s.login.or(s.username));
            let owner_repo = payload.repository.and_then(|r| r.full_name);
            (comment_body, pr_number, sender, owner_repo, comment_id)
        }
    };

    let cmd = match parse_gradient_command(&comment_body) {
        Some(cmd) => cmd,
        None => return,
    };
    let Some(pr_number) = pr_number else {
        warn!("comment webhook: /gradient command but no PR number");
        return;
    };
    let Some(sender) = sender_login else {
        warn!("comment webhook: /gradient command but no sender");
        return;
    };
    let Some(owner_repo) = owner_repo else {
        warn!("comment webhook: /gradient command but no repo path");
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
            let targets =
                resolve_github_app_targets(state, installation_id, &repo_urls, client_ip).await;
            if targets.is_empty() {
                warn!(installation_id, %owner_repo, "comment webhook (github): no integration matched");
                return;
            }
            targets
        }
    };

    let wildcard_override: Option<String> = match &cmd {
        GradientCommand::Approve => None,
        GradientCommand::Run { wildcard: None } => None,
        GradientCommand::Run {
            wildcard: Some(raw),
        } => match raw.parse::<Wildcard>() {
            Ok(_) => Some(raw.clone()),
            Err(e) => {
                warn!(wildcard = %raw, error = %e, "/gradient run wildcard rejected");
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

    let reaction_target = comment_id.map(|id| gradient_ci::ReactionTarget {
        owner: owner.to_string(),
        repo: repo.to_string(),
        pr_number,
        comment_id: id,
    });

    let mut action_taken = false;
    for integration_id in &integration_ids {
        let project_ids = match active_project_ids_for_integration(state, *integration_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "comment webhook: failed to load project list");
                continue;
            }
        };
        // Lift the maintainer trust probe out of the per-project loop: all
        // projects in this integration share the same forge auth, so the
        // answer is the same. This also lets us fire the eyes/confused
        // reaction exactly once per integration.
        let Some(probe_project) = first_project_with_reporter(state, &project_ids).await else {
            continue;
        };
        let is_maintainer = sender_is_trusted(state, probe_project, owner, repo, &sender).await;
        if let Some(target) = &reaction_target {
            let kind = if is_maintainer {
                gradient_ci::ReactionKind::Eyes
            } else {
                gradient_ci::ReactionKind::Confused
            };
            fire_reaction_via_project(state, probe_project, target, kind).await;
        }
        if !is_maintainer {
            warn!(
                %integration_id,
                pr_number,
                %sender,
                "Rejecting /gradient command - sender is not a repo writer"
            );
            continue;
        }

        let mut parked_unparked_in_integration = false;
        for project_id in &project_ids {
            let Ok(Some(eval)) =
                find_approval_gated_eval(&state.web_db, *project_id, pr_number).await
            else {
                continue;
            };
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
                        "PR approval gate cleared via /gradient comment"
                    );
                    if let Some(target) = &reaction_target {
                        let json = serde_json::json!({
                            "owner": target.owner,
                            "repo": target.repo,
                            "pr_number": target.pr_number,
                            "comment_id": target.comment_id,
                        });
                        if let Err(e) =
                            set_evaluation_source_comment(&state.web_db, unparked.id, json).await
                        {
                            warn!(error = %e, evaluation_id = %unparked.id, "stamp source_comment on unparked eval failed");
                        }
                    }
                    dispatch_approval_granted(state, &unparked).await;
                    if matches!(cmd, GradientCommand::Approve) {
                        submit_pr_approval_review(state, *project_id, owner, repo, pr_number).await;
                    }

                    parked_unparked_in_integration = true;
                    action_taken = true;
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(error = %e, evaluation_id = %eval.id, "Failed to unpark approval gate")
                }
            }
        }

        if parked_unparked_in_integration {
            continue;
        }
        if !matches!(cmd, GradientCommand::Run { .. }) {
            continue;
        }

        let Some(snapshot) = fetch_pr_snapshot(state, &project_ids, owner, repo, pr_number).await
        else {
            warn!(
                %integration_id,
                pr_number,
                "/gradient run: could not fetch PR head via any project reporter"
            );
            continue;
        };
        let commit_hash = match hex::decode(&snapshot.head_sha) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, sha = %snapshot.head_sha, "/gradient run: invalid head SHA");
                continue;
            }
        };
        // The maintainer running `/gradient run` was already trust-verified
        // above, so `sender` lets `decide_pr_gate` bypass the approval gate -
        // the command itself is the approval, even on a fork PR.
        let approval_ctx = PullRequestApprovalContext {
            pr_number: Some(pr_number),
            pr_author: None,
            is_fork: Some(snapshot.is_fork),
            sender: Some(sender.clone()),
        };
        let source_comment_json = reaction_target.as_ref().map(|t| {
            serde_json::json!({
                "owner": t.owner,
                "repo": t.repo,
                "pr_number": t.pr_number,
                "comment_id": t.comment_id,
            })
        });
        let outcome = trigger_pr_for_integration(
            state,
            scheduler,
            *integration_id,
            Some(snapshot.head_branch.as_str()),
            "synchronize",
            commit_hash,
            None,
            Some(sender.clone()),
            approval_ctx,
            snapshot.head_clone_url.clone(),
            true,
            wildcard_override.clone(),
            source_comment_json,
        )
        .await;
        if !outcome.queued.is_empty() {
            info!(
                %integration_id,
                pr_number,
                %sender,
                wildcard_override = wildcard_override.as_deref(),
                queued = outcome.queued.len(),
                "/gradient run created fresh evaluation"
            );
            action_taken = true;
        } else {
            debug!(
                %integration_id,
                pr_number,
                "/gradient run: trigger fan-out queued nothing"
            );
        }
    }
    if !action_taken {
        debug!(
            pr_number,
            "/gradient comment had no parked evaluation and produced no fresh fire"
        );
    }
}

/// Post a reaction on a PR/MR comment via the given project's reporter.
/// Best-effort: failures are logged and swallowed so the rest of the
/// command pipeline keeps running.
async fn fire_reaction_via_project(
    state: &Arc<ServerState>,
    project_id: ProjectId,
    target: &gradient_ci::ReactionTarget,
    kind: gradient_ci::ReactionKind,
) {
    let reporter = match gradient_ci::actions::reporter_for_project(&state.ci(), project_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            warn!(error = %e, %project_id, "/gradient reaction: resolving reporter");
            return;
        }
    };
    if let Err(e) = reporter.add_reaction(target, kind).await {
        warn!(error = %e, %project_id, ?kind, "/gradient reaction: post failed");
    }
}

/// Walk `project_ids` and return the first project that resolves to a usable
/// `CiReporter`. Used by the `/gradient run` fresh-eval path to obtain a
/// reporter for both the maintainer trust check and the PR-snapshot fetch.
async fn first_project_with_reporter(
    state: &Arc<ServerState>,
    project_ids: &[ProjectId],
) -> Option<ProjectId> {
    for project_id in project_ids {
        if let Ok(Some(_)) =
            gradient_ci::actions::reporter_for_project(&state.ci(), *project_id).await
        {
            return Some(*project_id);
        }
    }
    None
}

/// Fetch a [`PullRequestSnapshot`] using the first project's reporter.
/// Returns `None` if no project has a reporter, the reporter call errors, or
/// the PR is missing (closed/404).
async fn fetch_pr_snapshot(
    state: &Arc<ServerState>,
    project_ids: &[ProjectId],
    owner: &str,
    repo: &str,
    pr_number: u64,
) -> Option<gradient_ci::PullRequestSnapshot> {
    for project_id in project_ids {
        let reporter =
            match gradient_ci::actions::reporter_for_project(&state.ci(), *project_id).await {
                Ok(Some(r)) => r,
                _ => continue,
            };
        match reporter.get_pull_request(owner, repo, pr_number).await {
            Ok(Some(snap)) => return Some(snap),
            Ok(None) => return None,
            Err(e) => {
                warn!(error = %e, %project_id, "/gradient run: PR fetch failed, trying next project");
            }
        }
    }
    None
}

/// Posts a reply comment to the PR explaining a `/gradient run <wildcard>`
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
    parse_error: &gradient_types::input::InputError,
) {
    let body = format!(
        "Could not parse wildcard `{}`: {}",
        raw_wildcard, parse_error
    );

    for integration_id in integration_ids {
        let project_ids = match active_project_ids_for_integration(state, *integration_id).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, %integration_id, "/gradient run wildcard: failed to load projects for reply comment");
                continue;
            }
        };
        for project_id in project_ids {
            let reporter =
                match gradient_ci::actions::reporter_for_project(&state.ci(), project_id).await {
                    Ok(Some(r)) => r,
                    Ok(None) => continue,
                    Err(e) => {
                        warn!(error = %e, %project_id, "/gradient run wildcard: resolving reporter for reply comment");
                        continue;
                    }
                };
            match reporter.post_pr_comment(owner, repo, pr_number, &body).await {
                Ok(()) => return,
                Err(e) => {
                    warn!(error = %e, %project_id, "/gradient run wildcard: reply comment post failed, trying next project");
                }
            }
        }
    }
}

/// Outcome of parsing a `/gradient …` PR comment.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum GradientCommand {
    /// `/gradient run [wildcard]` - re-run CI for this PR. Unparks an
    /// existing approval-gated eval if one exists, otherwise creates a fresh
    /// evaluation. The optional `wildcard` overrides the project's default
    /// attribute path for this run; it is not yet validated, the caller must
    /// pass it through `Wildcard::from_str` and reply on parse failure.
    Run { wildcard: Option<String> },
    /// `/gradient approve` - explicitly clear the approval gate for this PR.
    /// No-op if there is no parked eval.
    Approve,
}

/// Lifts a `/gradient <subcommand>` instruction from a PR comment. The
/// command must appear on its own line (after trimming whitespace). Blank
/// lines and forge quote-reply lines (`> …`) are skipped so a maintainer
/// can quote PR context above the command; any other prose before or after
/// disqualifies the comment.
///
/// Recognised subcommands: `run [wildcard]` and `approve`.
pub(super) fn parse_gradient_command(body: &str) -> Option<GradientCommand> {
    const PREFIX: &str = "/gradient";

    let mut found: Option<GradientCommand> = None;
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
        if !rest.starts_with(|c: char| c.is_ascii_whitespace()) {
            return None;
        }
        let rest = rest.trim();
        let (verb, arg) = match rest.split_once(|c: char| c.is_ascii_whitespace()) {
            Some((v, a)) => (v, a.trim()),
            None => (rest, ""),
        };
        if verb.eq_ignore_ascii_case("run") {
            found = Some(GradientCommand::Run {
                wildcard: (!arg.is_empty()).then(|| arg.to_string()),
            });
            continue;
        }
        if verb.eq_ignore_ascii_case("approve") {
            if !arg.is_empty() {
                return None;
            }
            found = Some(GradientCommand::Approve);
            continue;
        }
        return None;
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
        GitHubInstallationPayload, GradientCommand, PullRequestApprovalContext, gate_decision,
        github_full_name, glob_match_pattern, glob_matches, normalize_repo_url,
        parse_gradient_command,
    };

    #[test]
    fn github_full_name_parses_every_url_form() {
        for url in [
            "https://github.com/NuschtOS/search.git",
            "https://github.com/NuschtOS/search",
            "git+https://github.com/NuschtOS/search",
            "git@github.com:NuschtOS/search.git",
            "github:NuschtOS/search",
            "git+github:NuschtOS/search",
        ] {
            assert_eq!(github_full_name(url).as_deref(), Some("nuschtos/search"), "{url}");
        }
    }

    #[test]
    fn github_full_name_rejects_non_github_hosts() {
        assert_eq!(github_full_name("https://gitlab.com/NuschtOS/search"), None);
        assert_eq!(github_full_name("https://gitea.example.com/acme/widgets"), None);
    }

    #[test]
    fn installation_payload_collects_full_names_from_both_arrays() {
        let created: GitHubInstallationPayload = serde_json::from_value(serde_json::json!({
            "action": "created",
            "installation": { "id": 1, "account": { "login": "NuschtOS" } },
            "sender": { "login": "tester" },
            "repositories": [
                { "full_name": "NuschtOS/search" },
                { "full_name": "NuschtOS/nixos-modules" },
            ],
        }))
        .unwrap();
        let names = created.installed_full_names();
        assert!(names.contains("nuschtos/search"));
        assert!(names.contains("nuschtos/nixos-modules"));

        let added: GitHubInstallationPayload = serde_json::from_value(serde_json::json!({
            "action": "added",
            "installation": { "id": 1, "account": { "login": "NuschtOS" } },
            "sender": { "login": "tester" },
            "repositories_added": [ { "full_name": "NuschtOS/ixx" } ],
        }))
        .unwrap();
        assert!(added.installed_full_names().contains("nuschtos/ixx"));
    }

    fn fork_ctx() -> PullRequestApprovalContext {
        PullRequestApprovalContext {
            pr_number: Some(7),
            pr_author: Some("external".into()),
            is_fork: Some(true),
            sender: Some("maintainer".into()),
        }
    }

    #[test]
    fn gate_same_repo_pr_bypasses() {
        let ctx = PullRequestApprovalContext {
            is_fork: Some(false),
            ..fork_ctx()
        };
        assert!(gate_decision(&ctx, false).is_none());
    }

    #[test]
    fn gate_fork_untrusted_sender_parks() {
        let gate = gate_decision(&fork_ctx(), false).expect("fork PR must park");
        assert_eq!(gate.pr_number, 7);
        assert_eq!(gate.pr_author, "external");
    }

    #[test]
    fn gate_fork_trusted_sender_bypasses() {
        assert!(
            gate_decision(&fork_ctx(), true).is_none(),
            "a trusted maintainer (force-push / command) must bypass the gate"
        );
    }

    #[test]
    fn gate_unknown_fork_status_fails_closed() {
        let ctx = PullRequestApprovalContext {
            is_fork: None,
            sender: None,
            ..fork_ctx()
        };
        assert!(
            gate_decision(&ctx, false).is_some(),
            "uncertain fork status with untrusted sender must park"
        );
    }

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
    fn parse_gradient_run_bare_returns_run_without_wildcard() {
        assert_eq!(
            parse_gradient_command("/gradient run"),
            Some(GradientCommand::Run { wildcard: None })
        );
        assert_eq!(
            parse_gradient_command("   /gradient run   "),
            Some(GradientCommand::Run { wildcard: None })
        );
        assert_eq!(
            parse_gradient_command("\n/gradient run\n"),
            Some(GradientCommand::Run { wildcard: None })
        );
    }

    #[test]
    fn parse_gradient_run_is_case_insensitive() {
        assert_eq!(
            parse_gradient_command("/GRADIENT Run"),
            Some(GradientCommand::Run { wildcard: None })
        );
        assert_eq!(
            parse_gradient_command("/Gradient RUN packages.*.*"),
            Some(GradientCommand::Run {
                wildcard: Some("packages.*.*".to_string())
            })
        );
    }

    #[test]
    fn parse_gradient_run_with_wildcard() {
        assert_eq!(
            parse_gradient_command("/gradient run packages.*.*"),
            Some(GradientCommand::Run {
                wildcard: Some("packages.*.*".to_string())
            })
        );
    }

    #[test]
    fn parse_gradient_run_with_complex_wildcard_preserves_raw() {
        let body = "/gradient run packages.*.foo,!packages.x86_64-linux.broken";
        let Some(GradientCommand::Run { wildcard: Some(w) }) = parse_gradient_command(body) else {
            panic!("expected Run with wildcard");
        };
        assert_eq!(w, "packages.*.foo,!packages.x86_64-linux.broken");
    }

    #[test]
    fn parse_gradient_run_trims_whitespace_around_wildcard() {
        let body = "   /gradient run   packages.*.*   ";
        let Some(GradientCommand::Run { wildcard: Some(w) }) = parse_gradient_command(body) else {
            panic!("expected Run with wildcard");
        };
        assert_eq!(w, "packages.*.*");
    }

    #[test]
    fn parse_gradient_run_after_quote_reply() {
        let body = "> @maintainer asked us to retrigger\n> after rebasing main\n\n/gradient run packages.*.*";
        let Some(GradientCommand::Run { wildcard: Some(w) }) = parse_gradient_command(body) else {
            panic!("expected Run with wildcard");
        };
        assert_eq!(w, "packages.*.*");
    }

    #[test]
    fn parse_gradient_approve() {
        assert_eq!(
            parse_gradient_command("/gradient approve"),
            Some(GradientCommand::Approve)
        );
        assert_eq!(
            parse_gradient_command("   /GRADIENT Approve   "),
            Some(GradientCommand::Approve)
        );
    }

    #[test]
    fn parse_gradient_approve_rejects_trailing_args() {
        assert!(parse_gradient_command("/gradient approve packages.*").is_none());
    }

    #[test]
    fn parse_gradient_rejects_unknown_subcommand() {
        assert!(parse_gradient_command("/gradient yolo").is_none());
        assert!(parse_gradient_command("/gradient").is_none());
    }

    #[test]
    fn parse_gradient_rejects_unrelated() {
        assert!(parse_gradient_command("looks good").is_none());
        assert!(parse_gradient_command("/gradientrun").is_none());
        assert!(parse_gradient_command("foo\n/gradient run\nbar").is_none());
        assert!(
            parse_gradient_command("quote-reply context\n\n/gradient run").is_none(),
            "non-quote prose before /gradient must reject"
        );
    }

    #[test]
    fn parse_gradient_legacy_ci_prefix_rejected() {
        // Hard rename: old /ci prefix no longer recognised.
        assert!(parse_gradient_command("/ci run").is_none());
        assert!(parse_gradient_command("/ci approve").is_none());
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
