/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::evaluation_rows::{EvaluationRows, load_evaluation_rows, load_project_name};
use super::matchers::requested_actions_for;
use crate::ActionError;
use crate::context::CiContext;
use crate::reporting::CommitStatusCheckWithIds;
use crate::{parse_owner_repo, reporting};
use anyhow::{Context, Result};
use gradient_db::evaluations::failed_attributes::failed_attributes;
use gradient_db::lookup::by_id;
use gradient_git_host::reporter::{CiReport, CiStatus};
use gradient_types::input::vec_to_hex;
use gradient_types::waiting_reason::WaitingReason;
use gradient_types::{
    BuildJobId, CEntryPoint, EBuildJob, EDerivation, EDerivationBuild, EEntryPoint, EEvaluation,
    EExecJob, EvaluationId, ExecJobId, MBuildJob, MCommit, MEvaluation, MTask,
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
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
pub(crate) async fn persist_evaluation_check_id(
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

pub(crate) async fn build_ci_reports_from_commit_status_check_rows(
    ctx: &CiContext,
    check: &CommitStatusCheckWithIds,
) -> Result<Vec<CiReport>> {
    let Some(evaluation) = by_id::<EEvaluation, _>(&ctx.db.worker_db, check.evaluation_id())
        .await
        .context("load evaluation")?
    else {
        return Ok(Vec::new());
    };
    let Some(state) = read_check_report_state(ctx, check, &evaluation).await? else {
        return Ok(Vec::new());
    };
    let EvaluationRows { task, commit } = load_evaluation_rows(ctx, &evaluation).await?;
    let target = build_report_target(ctx, &evaluation, &task, &commit).await?;

    Ok(match state {
        CheckReportRow::Approval {
            status,
            description,
        } => vec![target.report(
            reporting::approval_check_context(&task.name),
            status,
            description,
        )],
        CheckReportRow::Evaluation {
            status,
            description,
        } => evaluation_reports(ctx, &target, &task, status, description).await?,
        CheckReportRow::EntryPoint { name, status } => vec![target.report(
            reporting::build_check_context(&task.name, &name),
            status,
            None,
        )],
    })
}

enum CheckReportRow {
    Approval {
        status: CiStatus,
        description: Option<String>,
    },
    Evaluation {
        status: CiStatus,
        description: Option<String>,
    },
    EntryPoint {
        name: String,
        status: CiStatus,
    },
}

async fn read_check_report_state(
    ctx: &CiContext,
    check: &CommitStatusCheckWithIds,
    evaluation: &MEvaluation,
) -> Result<Option<CheckReportRow>> {
    let reason = evaluation
        .waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json);
    let description =
        reporting::queued_or_waiting_evaluation_description(evaluation.status, reason.clone())
            .map(str::to_owned);

    Ok(match check {
        CommitStatusCheckWithIds::Approval { .. } => {
            let status =
                reporting::ci_status_for_approval_check(evaluation.status, reason.as_ref());
            Some(CheckReportRow::Approval {
                description: description.filter(|_| status == CiStatus::ActionRequired),
                status,
            })
        }
        CommitStatusCheckWithIds::Evaluation { .. } => Some(CheckReportRow::Evaluation {
            status: reporting::ci_status_for_evaluation(&evaluation.status),
            description,
        }),
        CommitStatusCheckWithIds::Build { build_job, .. } => {
            load_build_job_check_state(ctx, evaluation, *build_job).await?
        }
        CommitStatusCheckWithIds::ExecJob {
            job,
            entry_point_name,
            ..
        } => read_exec_job_check_state_from_row(ctx, *job)
            .await?
            .map(|status| CheckReportRow::EntryPoint {
                name: entry_point_name.clone(),
                status,
            }),
    })
}

async fn load_build_job_check_state(
    ctx: &CiContext,
    evaluation: &MEvaluation,
    build_job: BuildJobId,
) -> Result<Option<CheckReportRow>> {
    let db = &ctx.db.worker_db;
    let Some(job) = by_id::<EBuildJob, _>(db, build_job)
        .await
        .context("load build_job")?
    else {
        return Ok(None);
    };
    if !will_evaluation_build_derivation(ctx, evaluation, &job).await? {
        return Ok(None);
    }
    let Some(status) = by_id::<EDerivationBuild, _>(db, job.derivation_build)
        .await
        .context("load derivation_build")?
        .and_then(|build| reporting::ci_status_for_build(&build.status))
    else {
        return Ok(None);
    };
    Ok(build_job_entry_point(ctx, &job)
        .await?
        .map(|name| CheckReportRow::EntryPoint { name, status }))
}

async fn read_exec_job_check_state_from_row(
    ctx: &CiContext,
    job: ExecJobId,
) -> Result<Option<CiStatus>> {
    Ok(by_id::<EExecJob, _>(&ctx.db.worker_db, job)
        .await
        .context("load exec_job")?
        .and_then(|job| reporting::ci_status_for_exec_job(job.status)))
}

async fn build_report_target<'a>(
    ctx: &CiContext,
    evaluation: &'a MEvaluation,
    task: &MTask,
    commit: &MCommit,
) -> Result<ReportTarget<'a>> {
    // Reports must target the task's base repository, not `evaluation.repository`. Fork PR
    // evaluations are pointing at the fork, where the GitHub App is missing and `/check-runs` is
    // returning 403.
    let (owner, repo) =
        parse_owner_repo(&task.repository).ok_or_else(|| ActionError::RepositoryUnparsable {
            repository: task.repository.clone(),
        })?;
    Ok(ReportTarget {
        evaluation,
        owner,
        repo,
        sha: vec_to_hex(&commit.hash),
        details_url: load_project_name(ctx, task.project).await.map(|project| {
            gradient_types::frontend_page_urls::evaluation(
                &ctx.db.config.server.frontend_url,
                &project,
                evaluation.id,
            )
        }),
    })
}

struct ReportTarget<'a> {
    evaluation: &'a MEvaluation,
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

async fn will_evaluation_build_derivation(
    ctx: &CiContext,
    evaluation: &MEvaluation,
    job: &MBuildJob,
) -> Result<bool> {
    if !evaluation.only_evaluate {
        return Ok(true);
    }
    Ok(by_id::<EDerivation, _>(&ctx.db.worker_db, job.derivation)
        .await
        .context("load derivation")?
        .is_some_and(|derivation| derivation.ifd))
}

async fn build_job_entry_point(ctx: &CiContext, job: &MBuildJob) -> Result<Option<String>> {
    let entry_point = EEntryPoint::find()
        .filter(CEntryPoint::Derivation.eq(job.derivation))
        .filter(CEntryPoint::Evaluation.eq(job.evaluation))
        .one(&ctx.db.worker_db)
        .await
        .context("load entry points")?;
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
        .context("load failed attributes")?;
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
