/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::matchers::requested_actions_for;
use crate::ci::context::CiContext;
use crate::ci::{parse_owner_repo, reporting};
use crate::forge::reporter::{CiReport, CiStatus};
use gradient_types::input::vec_to_hex;
use gradient_types::{
    BuildId, CEntryPoint, EBuild, ECommit, EEntryPoint, EEvaluation, EOrganization, EProject,
    EvaluationId,
};
use anyhow::{Context, Result, anyhow};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::Value as JsonValue;
use tracing::warn;

/// Atomically upsert `check_run_id` into `evaluation.check_run_ids` under
/// `context`. Uses Postgres `jsonb_set` so concurrent persists for
/// different context keys (e.g. Approval + Evaluation + per-Build) cannot
/// race each other into wiping previously-stored ids - a load-modify-write
/// over a JSON column would let the slower writer's snapshot clobber the
/// faster writer's entry.
pub(super) async fn persist_evaluation_check_id(
    ctx: &CiContext,
    evaluation_id: EvaluationId,
    context: &str,
    check_run_id: i64,
) {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let result = ctx
        .db
        .worker_db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"UPDATE evaluation
               SET check_run_ids = jsonb_set(
                   COALESCE(check_run_ids, '{}'::jsonb),
                   ARRAY[$2::text],
                   to_jsonb($3::bigint),
                   true
               )
               WHERE id = $1"#,
            [
                sea_orm::Value::Uuid(Some(Box::new(evaluation_id.into_inner()))),
                sea_orm::Value::String(Some(Box::new(context.to_string()))),
                sea_orm::Value::BigInt(Some(check_run_id)),
            ],
        ))
        .await;
    if let Err(e) = result {
        warn!(error = %e, %evaluation_id, "persisting evaluation check_run_ids");
    }
}

/// Read a check_run_id previously stored under `context` in
/// `evaluation.check_run_ids`.
fn check_run_id_for_context(eval: &gradient_types::MEvaluation, context: &str) -> Option<i64> {
    eval.check_run_ids
        .as_ref()
        .and_then(|v| v.as_object())
        .and_then(|m| m.get(context))
        .and_then(|v| v.as_i64())
}

/// Returns `Ok(None)` when the event is a per-build status update for a
/// build that has no `entry_point` row - those are intermediate dependency
/// builds, not user-visible CI targets, so we skip the forge POST.
pub(super) async fn build_ci_report_from_payload(
    ctx: &CiContext,
    event: &str,
    payload: &JsonValue,
    status: CiStatus,
) -> Result<Option<CiReport>> {
    let s = |k: &str| payload.get(k).and_then(|v| v.as_str()).map(String::from);

    let requested_actions = requested_actions_for(status.clone());

    if let (Some(owner), Some(repo), Some(sha), Some(context)) =
        (s("owner"), s("repo"), s("sha"), s("context"))
    {
        return Ok(Some(CiReport {
            owner,
            repo,
            sha,
            context,
            status,
            description: s("description"),
            details_url: s("details_url"),
            existing_check_id: payload.get("check_run_id").and_then(|v| v.as_i64()),
            requested_actions,
        }));
    }

    // `build_id` takes precedence over `evaluation_id` even when both are
    // present: the build-status dispatch (CiStatusReactor::on_build_terminal)
    // emits a payload carrying BOTH so downstream actions can correlate the
    // build to its eval, but the forge reporter must load the build so it
    // can pick the per-build check context. Falling back to the
    // evaluation-only path here would land every build event on the
    // Evaluation check.
    let (evaluation, build) = if let Some(bid) = s("build_id") {
        let build_id: BuildId = bid.parse().map_err(|_| anyhow!("invalid build_id"))?;
        let build = EBuild::find_by_id(build_id)
            .one(&ctx.db.worker_db)
            .await
            .context("loading build")?
            .ok_or_else(|| anyhow!("build {} not found", build_id))?;
        let evaluation = EEvaluation::find_by_id(build.evaluation)
            .one(&ctx.db.worker_db)
            .await
            .context("loading evaluation")?
            .ok_or_else(|| anyhow!("evaluation {} not found", build.evaluation))?;
        (evaluation, Some(build))
    } else if let Some(eid) = s("evaluation_id") {
        let evaluation_id: EvaluationId =
            eid.parse().map_err(|_| anyhow!("invalid evaluation_id"))?;
        let evaluation = EEvaluation::find_by_id(evaluation_id)
            .one(&ctx.db.worker_db)
            .await
            .context("loading evaluation")?
            .ok_or_else(|| anyhow!("evaluation {} not found", evaluation_id))?;
        (evaluation, None)
    } else {
        anyhow::bail!(
            "payload missing 'build_id', 'evaluation_id', and the full owner/repo/sha/context set"
        );
    };

    let project_id = evaluation
        .project
        .ok_or_else(|| anyhow!("evaluation has no project (direct build)"))?;

    let project = EProject::find_by_id(project_id)
        .one(&ctx.db.worker_db)
        .await
        .context("loading project")?
        .ok_or_else(|| anyhow!("project {} not found", project_id))?;

    let commit = ECommit::find_by_id(evaluation.commit)
        .one(&ctx.db.worker_db)
        .await
        .context("loading commit")?
        .ok_or_else(|| anyhow!("commit {} not found", evaluation.commit))?;

    // Always post check runs / status updates against the project's base
    // repository, not `evaluation.repository`. For fork PRs the evaluation
    // URL points at the fork (so the worker can fetch the commit), but the
    // GitHub App installation lives on the base repo - calling the fork's
    // /check-runs endpoint returns 403.
    let (owner, repo) = parse_owner_repo(&project.repository)
        .ok_or_else(|| anyhow!("could not parse owner/repo from {}", project.repository))?;

    let entry_points = match &build {
        Some(b) => EEntryPoint::find()
            .filter(CEntryPoint::Build.eq(b.id))
            .all(&ctx.db.worker_db)
            .await
            .context("loading entry points")?,
        None => Vec::new(),
    };

    // Only builds linked to a declared entry point get their own forge check.
    // Intermediate dependency builds (e.g. `__assert_fail-builder`) share the
    // entry-point check's status implicitly via the eval roll-up; emitting
    // one check per derivation would spam the PR with per-dependency noise.
    let entry_point_eval = entry_points.first().map(|ep| ep.eval.clone());

    let org_name = EOrganization::find_by_id(project.organization)
        .one(&ctx.db.worker_db)
        .await
        .ok()
        .flatten()
        .map(|o| o.name);

    // Pick the check-run name based on which phase fired the event so the
    // Approval, Evaluation, and per-Build checks each show as their own line
    // on the PR. A Build event for an intermediate dep (no entry_point row)
    // produces `None` so the caller can skip the report entirely.
    let context = match reporting::check_context_kind_for_event(event) {
        Some(reporting::CheckContextKind::Approval) => {
            reporting::approval_check_context(&project.name)
        }
        Some(reporting::CheckContextKind::Build) => match entry_point_eval.as_deref() {
            Some(label) => reporting::build_check_context(&project.name, label),
            None => return Ok(None),
        },
        Some(reporting::CheckContextKind::Evaluation) | None => {
            let wildcard_suffix =
                (evaluation.wildcard != project.wildcard).then_some(evaluation.wildcard.as_str());
            reporting::evaluation_check_context(&project.name, wildcard_suffix)
        }
    };

    let details_url = org_name.as_ref().map(|org| {
        format!(
            "{}/organization/{}/log/{}",
            ctx.db.config.server.frontend_url, org, evaluation.id
        )
    });

    Ok(Some(CiReport {
        owner,
        repo,
        sha: vec_to_hex(&commit.hash),
        context: context.clone(),
        status,
        description: s("description"),
        details_url,
        existing_check_id: check_run_id_for_context(&evaluation, &context)
            .or_else(|| payload.get("check_run_id").and_then(|v| v.as_i64())),
        requested_actions,
    }))
}
