/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::evaluation_rows::{EvaluationRows, load_evaluation_rows, load_project_name};
use super::matchers::requested_actions_for;
use crate::context::CiContext;
use crate::{parse_owner_repo, reporting};
use anyhow::{Context, Result, anyhow};
use gradient_db::evaluations::failed_attributes::failed_attributes;
use gradient_git_host::reporter::{CiReport, CiStatus};
use gradient_types::input::vec_to_hex;
use gradient_types::{BuildJobId, CEntryPoint, EBuildJob, EEntryPoint, EEvaluation, EvaluationId};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::Value as JsonValue;
use tracing::warn;

gradient_db::sql! {
    PERSIST_EVALUATION_CHECK_ID = r#"UPDATE evaluation
               SET check_run_ids = jsonb_set(
                   COALESCE(check_run_ids, '{}'::jsonb),
                   ARRAY[$2::text],
                   to_jsonb($3::bigint),
                   true
               )
               WHERE id = $1"#,
        params = [EvaluationId, Text("Evaluation"), Int(123456)];
}

/// `jsonb_set` is keeping concurrent persists for different context keys from wiping each other. A
/// load-modify-write over the JSON column would let the slower writer clobber the faster writer's
/// entry.
pub(super) async fn persist_evaluation_check_id(
    ctx: &CiContext,
    evaluation_id: EvaluationId,
    context: &str,
    check_run_id: i64,
) {
    use sea_orm::ConnectionTrait;

    let result = ctx
        .db
        .worker_db
        .execute_raw(PERSIST_EVALUATION_CHECK_ID.bind([
            sea_orm::Value::Uuid(Some(evaluation_id.into_inner())),
            sea_orm::Value::String(Some(context.to_string())),
            sea_orm::Value::BigInt(Some(check_run_id)),
        ]))
        .await;
    if let Err(e) = result {
        warn!(error = %e, %evaluation_id, "persisting evaluation check_run_ids");
    }
}

fn check_run_id_for_context(eval: &gradient_types::MEvaluation, context: &str) -> Option<i64> {
    eval.check_run_ids
        .as_ref()
        .and_then(|v| v.as_object())
        .and_then(|m| m.get(context))
        .and_then(|v| v.as_i64())
}

pub(super) async fn build_ci_reports_from_payload(
    ctx: &CiContext,
    event: &str,
    payload: &JsonValue,
    status: CiStatus,
) -> Result<Vec<CiReport>> {
    let s = |k: &str| payload.get(k).and_then(|v| v.as_str()).map(String::from);

    if let (Some(owner), Some(repo), Some(sha), Some(context)) =
        (s("owner"), s("repo"), s("sha"), s("context"))
    {
        return Ok(vec![CiReport {
            owner,
            repo,
            sha,
            context,
            requested_actions: requested_actions_for(status.clone()),
            status,
            description: s("description"),
            details_url: s("details_url"),
            existing_check_id: payload.get("check_run_id").and_then(|v| v.as_i64()),
        }]);
    }

    let (evaluation, build_job) = load_subject(ctx, payload).await?;
    let EvaluationRows { task, commit } = load_evaluation_rows(ctx, &evaluation).await?;

    // Reports must target the task's base repository, not `evaluation.repository`. Fork PR
    // evaluations are pointing at the fork, where the GitHub App is missing and `/check-runs` is
    // returning 403.
    let (owner, repo) = parse_owner_repo(&task.repository)
        .ok_or_else(|| anyhow!("could not parse owner/repo from {}", task.repository))?;
    let target = ReportTarget {
        evaluation: &evaluation,
        owner,
        repo,
        sha: vec_to_hex(&commit.hash),
        details_url: load_project_name(ctx, task.project).await.map(|project| {
            format!(
                "{}/project/{}/log/{}",
                ctx.db.config.server.frontend_url, project, evaluation.id
            )
        }),
    };
    let description = s("description");
    let payload_check_id = payload.get("check_run_id").and_then(|v| v.as_i64());

    let mut reports = match reporting::check_context_kind_for_event(event) {
        Some(reporting::CheckContextKind::Approval) => vec![target.report(
            reporting::approval_check_context(&task.name),
            status,
            description,
        )],
        Some(reporting::CheckContextKind::Build) => {
            match build_job_entry_point(ctx, build_job.as_ref()).await? {
                Some(label) => vec![target.report(
                    reporting::build_check_context(&task.name, &label),
                    status,
                    description,
                )],
                None => Vec::new(),
            }
        }
        Some(reporting::CheckContextKind::Evaluation) | None => {
            evaluation_reports(ctx, &target, &task, status, description).await?
        }
    };
    if let Some(first) = reports.first_mut() {
        first.existing_check_id = first.existing_check_id.or(payload_check_id);
    }
    Ok(reports)
}

struct ReportTarget<'a> {
    evaluation: &'a gradient_types::MEvaluation,
    owner: String,
    repo: String,
    sha: String,
    details_url: Option<String>,
}

impl ReportTarget<'_> {
    fn report(&self, context: String, status: CiStatus, description: Option<String>) -> CiReport {
        CiReport {
            owner: self.owner.clone(),
            repo: self.repo.clone(),
            sha: self.sha.clone(),
            existing_check_id: check_run_id_for_context(self.evaluation, &context),
            context,
            requested_actions: requested_actions_for(status.clone()),
            status,
            description,
            details_url: self.details_url.clone(),
        }
    }
}

async fn load_subject(
    ctx: &CiContext,
    payload: &JsonValue,
) -> Result<(
    gradient_types::MEvaluation,
    Option<gradient_types::MBuildJob>,
)> {
    let s = |k: &str| payload.get(k).and_then(|v| v.as_str());

    if let Some(bid) = s("build_id") {
        let build_job_id: BuildJobId = bid.parse().map_err(|_| anyhow!("invalid build_id"))?;
        let build_job = EBuildJob::find_by_id(build_job_id)
            .one(&ctx.db.worker_db)
            .await
            .context("loading build_job")?
            .ok_or_else(|| anyhow!("build_job {} not found", build_job_id))?;
        let evaluation = EEvaluation::find_by_id(build_job.evaluation)
            .one(&ctx.db.worker_db)
            .await
            .context("loading evaluation")?
            .ok_or_else(|| anyhow!("evaluation {} not found", build_job.evaluation))?;
        return Ok((evaluation, Some(build_job)));
    }

    let Some(eid) = s("evaluation_id") else {
        anyhow::bail!(
            "payload missing 'build_id', 'evaluation_id', and the full owner/repo/sha/context set"
        );
    };
    let evaluation_id: EvaluationId = eid.parse().map_err(|_| anyhow!("invalid evaluation_id"))?;
    let evaluation = EEvaluation::find_by_id(evaluation_id)
        .one(&ctx.db.worker_db)
        .await
        .context("loading evaluation")?
        .ok_or_else(|| anyhow!("evaluation {} not found", evaluation_id))?;
    Ok((evaluation, None))
}

async fn build_job_entry_point(
    ctx: &CiContext,
    build_job: Option<&gradient_types::MBuildJob>,
) -> Result<Option<String>> {
    let Some(b) = build_job else {
        return Ok(None);
    };
    let entry_point = EEntryPoint::find()
        .filter(CEntryPoint::Derivation.eq(b.derivation))
        .filter(CEntryPoint::Evaluation.eq(b.evaluation))
        .one(&ctx.db.worker_db)
        .await
        .context("loading entry points")?;
    Ok(entry_point.map(|ep| ep.eval))
}

async fn evaluation_reports(
    ctx: &CiContext,
    target: &ReportTarget<'_>,
    task: &gradient_types::MTask,
    status: CiStatus,
    description: Option<String>,
) -> Result<Vec<CiReport>> {
    let evaluation = target.evaluation;
    if reporting::suppress_evaluation_failure(&status, evaluation.building_started_at.is_some()) {
        return Ok(Vec::new());
    }

    let failed = failed_attributes(&ctx.db.worker_db, evaluation.id)
        .await
        .context("loading failed attributes")?;
    let status = reporting::evaluation_check_status(status, !failed.is_empty());
    let attributes_failed = status == CiStatus::Failure && !failed.is_empty();

    let wildcard_suffix =
        (evaluation.wildcard != task.wildcard).then_some(evaluation.wildcard.as_str());
    let description = if attributes_failed {
        Some(reporting::failed_attributes_description(failed.len()))
    } else {
        description
    };
    let mut reports = vec![target.report(
        reporting::evaluation_check_context(&task.name, wildcard_suffix),
        status,
        description,
    )];
    if attributes_failed {
        reports.extend(failed.iter().map(|f| {
            target.report(
                reporting::build_check_context(&task.name, &f.attr),
                CiStatus::Failure,
                Some("Failed to evaluate".to_owned()),
            )
        }));
    }
    Ok(reports)
}
