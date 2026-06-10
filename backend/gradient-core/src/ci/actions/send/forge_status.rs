/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::ci::actions::crypto::decrypt_secret_with_file;
use crate::ci::actions::matchers::forge_status_for_event;
use crate::ci::actions::report::{build_ci_report_from_payload, persist_evaluation_check_id};
use crate::ci::actions::ExecutorOk;
use crate::ci::context::CiContext;
use crate::ci::integration_lookup::IntegrationKind;
use gradient_types::ForgeType;
use gradient_forge::reporter::{CiReporter, GithubAppReporter};
use gradient_types::{
    ActionConfig, ActionType, CIntegration, CProjectAction, EIntegration, EOrganization,
    EProjectAction, EvaluationId, IntegrationId, ProjectId,
};
use anyhow::{Context, Result, anyhow};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::Value as JsonValue;
use std::sync::Arc;

pub(crate) async fn execute_forge_status_report(
    ctx: &CiContext,
    event: &str,
    payload: &JsonValue,
    integration_id: IntegrationId,
) -> Result<ExecutorOk> {
    let ci_status = forge_status_for_event(event)
        .ok_or_else(|| anyhow!("event '{}' has no forge status mapping", event))?;

    let Some(report) = build_ci_report_from_payload(ctx, event, payload, ci_status).await? else {
        // Build event for an intermediate dependency (no entry_point row).
        // Nothing to post - the entry-point check covers the user-visible
        // status.
        return Ok(ExecutorOk {
            status_code: Some(204),
            response_body: None,
        });
    };
    let context_key = report.context.clone();
    let reporter = build_reporter_for_integration(ctx, integration_id).await?;
    let new_id = reporter
        .report(&report)
        .await
        .context("forge status report failed")?;
    if let (Some(new_id), Some(eid)) = (
        new_id,
        payload.get("evaluation_id").and_then(|v| v.as_str()),
    ) && let Ok(evaluation_id) = eid.parse::<EvaluationId>()
    {
        persist_evaluation_check_id(ctx, evaluation_id, &context_key, new_id).await;
    }
    let body = new_id.map(|id| format!("{{\"check_run_id\":{}}}", id));
    Ok(ExecutorOk {
        status_code: Some(200),
        response_body: body,
    })
}

/// Find the project's first active `ForgeStatusReport` action and build a
/// `CiReporter` from its integration. Used by the PR-approval trust probe to
/// reuse the same forge credentials Actions already use for status reporting.
pub async fn reporter_for_project(
    ctx: &CiContext,
    project_id: ProjectId,
) -> Result<Option<Arc<dyn CiReporter>>> {
    let action = EProjectAction::find()
        .filter(CProjectAction::Project.eq(project_id))
        .filter(CProjectAction::Active.eq(true))
        .filter(CProjectAction::ActionType.eq(ActionType::ForgeStatusReport.to_i16()))
        .one(&ctx.db.worker_db)
        .await
        .context("loading forge_status_report action")?;
    let Some(action) = action else {
        return Ok(None);
    };
    let cfg: ActionConfig =
        serde_json::from_value(action.config.clone()).context("decoding action config")?;
    let ActionConfig::ForgeStatusReport { integration_id } = cfg else {
        return Ok(None);
    };
    Ok(Some(
        build_reporter_for_integration(ctx, integration_id).await?,
    ))
}

async fn build_reporter_for_integration(
    ctx: &CiContext,
    integration_id: IntegrationId,
) -> Result<Arc<dyn CiReporter>> {
    let integration = EIntegration::find_by_id(integration_id)
        .filter(CIntegration::Kind.eq(i16::from(IntegrationKind::Outbound)))
        .one(&ctx.db.worker_db)
        .await
        .context("loading integration")?
        .ok_or_else(|| anyhow!("outbound integration {} not found", integration_id))?;

    let forge = ForgeType::try_from(integration.forge_type)
        .map_err(|_| anyhow!("integration has unknown forge_type"))?;
    let provider = ctx
        .forge
        .get(forge)
        .ok_or_else(|| anyhow!("no forge provider registered for {:?}", forge))?;

    let token = match integration.access_token.as_deref() {
        Some(enc) => Some(
            decrypt_secret_with_file(&ctx.db.config.secrets.crypt_secret_file, enc)
                .map_err(|e| anyhow!("decrypt integration token: {}", e))?,
        ),
        None => None,
    };

    if provider.supports_app_auth()
        && let Some(reporter) = build_github_app_reporter(ctx, &integration).await?
    {
        return Ok(reporter);
    }

    provider.build_reporter(
        ctx.http.clone(),
        integration.endpoint_url.as_deref(),
        token.as_ref().map(|t| t.expose()),
    )
}

/// GitHub-App installation reporter, used when the App is configured and the
/// integration's org has an installation. Returns `None` to fall back to the
/// provider's token reporter.
async fn build_github_app_reporter(
    ctx: &CiContext,
    integration: &gradient_entity::integration::Model,
) -> Result<Option<Arc<dyn CiReporter>>> {
    let Some(github_app) = ctx.db.config.github_app.clone() else {
        return Ok(None);
    };
    let project_org = EOrganization::find_by_id(integration.organization)
        .one(&ctx.db.worker_db)
        .await
        .context("loading organization for github app")?
        .ok_or_else(|| anyhow!("integration organization not found"))?;
    let Some(installation_id) = project_org.github_installation_id else {
        return Ok(None);
    };
    let pem = std::fs::read_to_string(&github_app.private_key_file)
        .context("reading github app private key")?;
    let r = GithubAppReporter::new(ctx.http.clone(), "", github_app.app_id, pem, installation_id)?;
    Ok(Some(Arc::new(r)))
}
