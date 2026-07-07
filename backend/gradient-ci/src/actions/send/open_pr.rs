/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `OpenPr` executor: reads the worker-produced candidate lock for an
//! `input_update` evaluation, commits it onto a deterministic branch, and opens
//! or updates a pull request, recording the lifecycle in `open_pr_state`.

use super::forge_status::build_reporter_for_integration;
use crate::actions::ExecutorOk;
use crate::context::CiContext;
use anyhow::{Context, Result, anyhow};
use gradient_forge::reporter::parse_owner_repo;
use gradient_forge::{BranchCommit, CommitFile, CommitIdent};
use gradient_types::{
    AOpenPrState, CEvaluationInputUpdate, COpenPrState, ECommit, EEvaluation,
    EEvaluationInputUpdate, EOpenPrState, EProject, EvaluationId, IntegrationId, OpenPrStateId,
    ProjectActionId, ProjectId,
};
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use serde::Deserialize;
use serde_json::Value as JsonValue;

#[derive(Deserialize)]
struct BumpRow {
    name: String,
    old_rev: Option<String>,
    new_rev: String,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_open_pr(
    ctx: &CiContext,
    _event: &str,
    payload: &JsonValue,
    action_id: ProjectActionId,
    project_id: ProjectId,
    integration_id: IntegrationId,
    branch_pattern: &str,
    title_template: Option<&str>,
    body_template: Option<&str>,
    update_existing: bool,
) -> Result<ExecutorOk> {
    let evaluation_id: EvaluationId = payload
        .get("evaluation_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("open_pr payload missing evaluation_id"))?
        .parse()
        .map_err(|_| anyhow!("invalid evaluation_id"))?;

    let sidecar = EEvaluationInputUpdate::find()
        .filter(CEvaluationInputUpdate::Evaluation.eq(evaluation_id))
        .one(&ctx.db.worker_db)
        .await
        .context("loading input_update sidecar")?;
    let Some(sidecar) = sidecar else {
        return Ok(no_op());
    };
    let Some(candidate_lock) = sidecar.candidate_lock else {
        return Ok(no_op());
    };

    let bumps: Vec<BumpRow> = sidecar
        .bumped_inputs
        .as_ref()
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()
        .context("decoding bumped_inputs")?
        .unwrap_or_default();
    if bumps.is_empty() {
        return Ok(no_op());
    }

    let project = EProject::find_by_id(project_id)
        .one(&ctx.db.worker_db)
        .await
        .context("loading project")?
        .ok_or_else(|| anyhow!("project {project_id} not found"))?;
    let (owner, repo) = parse_owner_repo(&project.repository)
        .ok_or_else(|| anyhow!("cannot parse owner/repo from {}", project.repository))?;

    let reporter = build_reporter_for_integration(ctx, integration_id).await?;
    let base_branch = reporter
        .default_branch(&owner, &repo)
        .await
        .context("resolving repository default branch")?;

    let primary_input = bumps.first().map(|b| b.name.as_str()).unwrap_or("inputs");
    let branch = branch_pattern.replace("{input}", primary_input);

    let title = render_template(title_template, &bumps).unwrap_or_else(|| default_title(&bumps));
    let body = render_template(body_template, &bumps).unwrap_or_else(|| default_body(&bumps));

    if !update_existing {
        let existing = EOpenPrState::find()
            .filter(COpenPrState::Action.eq(action_id))
            .filter(COpenPrState::Branch.eq(branch.clone()))
            .one(&ctx.db.worker_db)
            .await
            .context("checking existing open_pr_state")?;
        if existing.as_ref().and_then(|s| s.forge_pr_number).is_some() {
            return Ok(no_op());
        }
    }

    let commit = BranchCommit {
        message: title.clone(),
        author: configured_commit_ident(
            &ctx.db.config.server.pr_commit_name,
            &ctx.db.config.server.pr_commit_email,
        ),
        files: vec![CommitFile {
            path: "flake.lock".into(),
            contents: candidate_lock.into_bytes(),
        }],
    };

    let head_commit = reporter
        .upsert_branch(&owner, &repo, &branch, &base_branch, &commit)
        .await
        .context("committing candidate lock to branch")?;

    let pr = reporter
        .open_or_update_pr(&owner, &repo, &branch, &base_branch, &title, &body)
        .await
        .context("opening pull request")?;

    upsert_open_pr_state(ctx, project_id, action_id, &branch, pr.number, &head_commit).await?;
    point_eval_at_pushed_commit(ctx, evaluation_id, &head_commit, &title).await?;

    Ok(ExecutorOk {
        status_code: Some(200),
        response_body: Some(format!(
            "{{\"pr_number\":{},\"branch\":\"{}\"}}",
            pr.number, branch
        )),
    })
}

fn no_op() -> ExecutorOk {
    ExecutorOk {
        status_code: Some(204),
        response_body: None,
    }
}

/// The identity to force onto the commit, or `None` to let the forge attribute
/// it to the authenticated app/token. Both fields must be set; a half-configured
/// identity falls back to `None`.
fn configured_commit_ident(name: &Option<String>, email: &Option<String>) -> Option<CommitIdent> {
    match (name.as_deref(), email.as_deref()) {
        (Some(n), Some(e)) if !n.is_empty() && !e.is_empty() => Some(CommitIdent {
            name: n.to_owned(),
            email: e.to_owned(),
        }),
        _ => None,
    }
}

async fn upsert_open_pr_state(
    ctx: &CiContext,
    project_id: ProjectId,
    action_id: ProjectActionId,
    branch: &str,
    pr_number: i64,
    head_commit: &str,
) -> Result<()> {
    let now = gradient_types::now();
    let existing = EOpenPrState::find()
        .filter(COpenPrState::Action.eq(action_id))
        .filter(COpenPrState::Branch.eq(branch))
        .one(&ctx.db.worker_db)
        .await
        .context("loading open_pr_state")?;

    if let Some(row) = existing {
        let mut am = row.into_active_model();
        am.forge_pr_number = Set(Some(pr_number));
        am.head_commit = Set(Some(head_commit.to_owned()));
        am.status = Set("open".to_owned());
        am.updated_at = Set(now);
        am.update(&ctx.db.worker_db)
            .await
            .context("updating open_pr_state")?;
    } else {
        AOpenPrState {
            id: Set(OpenPrStateId::now_v7()),
            project: Set(project_id),
            action: Set(action_id),
            branch: Set(branch.to_owned()),
            forge_pr_number: Set(Some(pr_number)),
            head_commit: Set(Some(head_commit.to_owned())),
            status: Set("open".to_owned()),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&ctx.db.worker_db)
        .await
        .context("inserting open_pr_state")?;
    }

    Ok(())
}

/// Repoint the `input_update` evaluation at the flake.lock-update commit pushed
/// to the PR branch, so the project shows the generated commit instead of the
/// unrelated base commit it was seeded from.
async fn point_eval_at_pushed_commit(
    ctx: &CiContext,
    evaluation_id: EvaluationId,
    head_commit: &str,
    message: &str,
) -> Result<()> {
    let Ok(hash) = hex::decode(head_commit) else {
        return Ok(());
    };

    let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&ctx.db.worker_db)
        .await
        .context("loading evaluation")?
    else {
        return Ok(());
    };
    let Some(commit) = ECommit::find_by_id(eval.commit)
        .one(&ctx.db.worker_db)
        .await
        .context("loading evaluation commit")?
    else {
        return Ok(());
    };

    let mut am = commit.into_active_model();
    am.hash = Set(hash);
    am.message = Set(message.to_owned());
    am.author_name = Set(ctx
        .db
        .config
        .server
        .pr_commit_name
        .clone()
        .unwrap_or_else(|| "Gradient".to_owned()));
    am.update(&ctx.db.worker_db)
        .await
        .context("updating evaluation commit")?;

    Ok(())
}

fn input_summary(bumps: &[BumpRow]) -> String {
    bumps
        .iter()
        .map(|b| b.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn default_title(bumps: &[BumpRow]) -> String {
    format!("flake.lock: update {}", input_summary(bumps))
}

fn default_body(bumps: &[BumpRow]) -> String {
    let mut body = String::from("Automated flake.lock update.\n\n");
    for b in bumps {
        let from = b.old_rev.as_deref().map(short_rev).unwrap_or("(new)");
        body.push_str(&format!(
            "- `{}`: {} -> {}\n",
            b.name,
            from,
            short_rev(&b.new_rev)
        ));
    }

    body
}

fn render_template(template: Option<&str>, bumps: &[BumpRow]) -> Option<String> {
    let t = template?;
    let primary = bumps.first().map(|b| b.name.as_str()).unwrap_or("");

    Some(
        t.replace("{input}", primary)
            .replace("{inputs}", &input_summary(bumps))
            .replace("{count}", &bumps.len().to_string()),
    )
}

fn short_rev(rev: &str) -> &str {
    &rev[..rev.len().min(12)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_ident_needs_both_fields() {
        assert_eq!(configured_commit_ident(&None, &None), None);
        assert_eq!(configured_commit_ident(&Some("Bot".into()), &None), None);
        assert_eq!(configured_commit_ident(&None, &Some("b@x".into())), None);
        assert_eq!(
            configured_commit_ident(&Some(String::new()), &Some("b@x".into())),
            None
        );
    }

    #[test]
    fn commit_ident_set_when_both_present() {
        assert_eq!(
            configured_commit_ident(&Some("Bot".into()), &Some("b@x".into())),
            Some(CommitIdent {
                name: "Bot".into(),
                email: "b@x".into()
            })
        );
    }
}
