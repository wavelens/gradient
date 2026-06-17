/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Project Actions dispatch and execution. This module fans build/evaluation
//! events out to the configured actions ([`dispatch_event`]); the execution and
//! per-config executors live in [`executor`] and [`send`].

mod crypto;
mod executor;
mod matchers;
mod payload;
mod report;
mod send;

use crate::context::CiContext;
use gradient_types::{ActionType, CProjectAction, EProjectAction, ProjectId};
use serde_json::Value as JsonValue;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tracing::{error, warn};

pub use crypto::{
    decrypt_action_secret, decrypt_secret_with_file, encrypt_action_secret, encrypt_secret_with_file,
};
pub use executor::execute_action;
pub use matchers::{FORGE_STATUS_EVENTS, forge_status_for_event, matches_event};
pub use payload::forge_status_payload;
pub use send::{reporter_for_project, verify_forge_action};

pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Successful action delivery: the executor's HTTP/SMTP status and any response
/// body, recorded on the `project_action_delivery` row.
pub(crate) struct ExecutorOk {
    pub(crate) status_code: Option<i32>,
    pub(crate) response_body: Option<String>,
}

fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        if let Some((idx, _)) = s.char_indices().take_while(|(i, _)| *i <= max).last() {
            s.truncate(idx);
        } else {
            s.truncate(max);
        }
    }
    s
}

pub async fn dispatch_evaluation_event(
    ctx: &CiContext,
    project_id: ProjectId,
    event: &str,
    payload: JsonValue,
) {
    dispatch_event(ctx, project_id, event, payload).await;
}

/// Dispatch the first forge-status event for a freshly-created evaluation.
///
/// Replaces the legacy `spawn_pending_ci_for_eval` reporter that was removed
/// alongside the per-project outbound integration: an eval row that has just
/// been INSERTed (Queued, or Waiting+Approval/NoCache/Workers due to the
/// trigger-time gates) never transitions through `update_evaluation_status`,
/// so the terminal-status reactor would not fire for it. Without
/// this helper, the commit shows no Gradient check at all until an eval
/// worker actually starts processing it.
///
/// Maps the eval's creation-time state to the matching event:
/// - `Queued` → `evaluation.queued` (Pending).
/// - `Waiting + Approval` → `evaluation.action_required` (ActionRequired,
///   carries the maintainer-approval description).
/// - `Waiting + NoCache` → `evaluation.queued` (Pending, "no cache" description).
/// - `Waiting + Workers` → `evaluation.queued` (Pending, "no eval-capable
///   worker" description). Issue #268.
pub async fn dispatch_evaluation_created(ctx: &CiContext, eval: &gradient_types::MEvaluation) {
    use gradient_types::waiting_reason::WaitingReason;
    use gradient_entity::evaluation::EvaluationStatus;

    let Some(project_id) = eval.project else {
        return;
    };

    let reason = eval
        .waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json);

    let (event, description) = match (eval.status, reason) {
        (EvaluationStatus::Queued, _) => ("evaluation.queued", None),
        (EvaluationStatus::Waiting, Some(WaitingReason::Approval { .. })) => (
            "evaluation.action_required",
            Some("Awaiting maintainer approval for external contributor PR."),
        ),
        (EvaluationStatus::Waiting, Some(WaitingReason::NoCache)) => (
            "evaluation.queued",
            Some("Waiting for a writable cache subscription before this evaluation can run."),
        ),
        (EvaluationStatus::Waiting, Some(WaitingReason::CacheStorageFull)) => (
            "evaluation.queued",
            Some("Waiting for cache storage to free up before this evaluation can run."),
        ),
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::Workers {
                connected_workers: 0,
                ..
            }),
        ) => (
            "evaluation.queued",
            Some("Waiting for an eval-capable worker to be registered on the organisation."),
        ),
        _ => return,
    };

    let mut payload = serde_json::json!({
        "evaluation_id": eval.id,
        "project_id": eval.project,
        "repository": eval.repository,
        "status": event,
    });
    if let Some(text) = description {
        payload["description"] = JsonValue::String(text.to_string());
    }

    dispatch_evaluation_event(ctx, project_id, event, payload).await;
}

pub async fn dispatch_build_event(
    ctx: &CiContext,
    project_id: ProjectId,
    event: &str,
    payload: JsonValue,
) {
    dispatch_event(ctx, project_id, event, payload).await;
}

async fn dispatch_event(ctx: &CiContext, project_id: ProjectId, event: &str, payload: JsonValue) {
    let actions = match EProjectAction::find()
        .filter(CProjectAction::Project.eq(project_id))
        .filter(CProjectAction::Active.eq(true))
        .all(&ctx.db.worker_db)
        .await
    {
        Ok(a) => a,
        Err(e) => {
            error!(error = %e, %project_id, "Failed to load project actions");
            return;
        }
    };

    for action in actions {
        if !matches_event(&action, event) {
            continue;
        }
        // `OpenPr` fires on a normal gate event (build/eval completed) but must
        // only act on `input_update` evaluations, never regular CI runs.
        if action.action_type == ActionType::OpenPr.to_i16()
            && payload.get("evaluation_kind").and_then(|v| v.as_str()) != Some("input_update")
        {
            continue;
        }

        let ctx = ctx.clone();
        let payload = payload.clone();
        let event = event.to_string();
        tokio::spawn(async move {
            if let Err(e) = execute_action(&ctx, action, &event, payload).await {
                warn!(error = %e, "Action execution failed");
            }
        });
    }
}

#[cfg(test)]
mod tests;
