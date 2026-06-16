/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::send::{
    execute_forge_status_report, execute_open_pr, execute_send_mail, execute_send_web_request,
};
use super::{MAX_BODY_BYTES, truncate};
use crate::context::CiContext;
use gradient_types::{
    ActionConfig, MProjectAction, MProjectActionDelivery, ProjectActionDeliveryId,
};
use anyhow::{Context, Result};
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, IntoActiveModel};
use serde_json::Value as JsonValue;
use std::time::Instant;
use tracing::warn;

pub async fn execute_action(
    ctx: &CiContext,
    action: MProjectAction,
    event: &str,
    payload: JsonValue,
) -> Result<()> {
    let cfg: ActionConfig =
        serde_json::from_value(action.config.clone()).context("decoding action config")?;
    let action_id_for_pr = action.id;
    let project_for_pr = action.project;
    let started = Instant::now();
    let request_body = truncate(
        serde_json::to_string(&payload).unwrap_or_default(),
        MAX_BODY_BYTES,
    );

    let result = match cfg {
        ActionConfig::SendMail {
            recipients,
            subject_template,
        } => {
            execute_send_mail(
                ctx,
                event,
                &payload,
                &recipients,
                subject_template.as_deref(),
            )
            .await
        }
        ActionConfig::SendWebRequest { url, token } => {
            execute_send_web_request(ctx, event, &payload, &url, token.as_deref()).await
        }
        ActionConfig::ForgeStatusReport { integration_id } => {
            execute_forge_status_report(ctx, event, &payload, integration_id).await
        }
        ActionConfig::OpenPr {
            integration_id,
            branch_pattern,
            title_template,
            body_template,
            update_existing,
            ..
        } => {
            execute_open_pr(
                ctx,
                event,
                &payload,
                action_id_for_pr,
                project_for_pr,
                integration_id,
                &branch_pattern,
                title_template.as_deref(),
                body_template.as_deref(),
                update_existing,
            )
            .await
        }
    };

    let duration_ms = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);
    let success = match &result {
        Ok(ok) => ok
            .status_code
            .map(|c| (200..300).contains(&c))
            .unwrap_or(true),
        Err(_) => false,
    };
    let (response_status, response_body, error_message) = match &result {
        Ok(ok) => (ok.status_code, ok.response_body.clone(), None),
        Err(e) => (None, None, Some(format!("{:#}", e))),
    };

    let action_id = action.id;
    let delivery = MProjectActionDelivery {
        id: ProjectActionDeliveryId::now_v7(),
        action_id,
        event: event.to_string(),
        request_body,
        response_status,
        response_body: response_body.map(|s| truncate(s, MAX_BODY_BYTES)),
        error_message,
        success,
        duration_ms,
        delivered_at: gradient_types::now(),
    }
    .into_active_model();

    if let Err(e) = delivery.insert(&ctx.db.worker_db).await {
        warn!(error = %e, %action_id, "Failed to record action delivery");
    }

    if success {
        let mut am = sea_orm::IntoActiveModel::into_active_model(action);
        am.last_fired_at = Set(Some(gradient_types::now()));
        am.updated_at = Set(gradient_types::now());
        if let Err(e) = am.update(&ctx.db.worker_db).await {
            warn!(error = %e, %action_id, "Failed to update action last_fired_at");
        }
    }

    result.map(|_| ())
}
