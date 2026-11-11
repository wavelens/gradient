/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::IncomingWebhookResponse;
use crate::error::WebResult;
use gradient_db::WebDb;
use gradient_entity::incoming_webhook_delivery::{DeliveryResult, Model};
use gradient_types::{GitHostConnectionId, IncomingWebhookDeliveryId};
use tracing::warn;

const MAX_EVENT_NAME_LENGTH: usize = 64;

#[derive(Default)]
pub(super) struct DeliveryLog {
    pub(super) connections: Vec<GitHostConnectionId>,
    pub(super) event: Option<String>,
}

impl DeliveryLog {
    pub(super) fn set_connection_and_event(
        &mut self,
        connection: GitHostConnectionId,
        event: &str,
    ) {
        self.connections.push(connection);
        self.event = Some(event.to_owned());
    }

    pub(super) async fn record(&self, db: &WebDb, result: &WebResult<IncomingWebhookResponse>) {
        if self.connections.is_empty() {
            return;
        }
        let entry = entry(self.event.as_deref(), result);
        for connection in &self.connections {
            let row = Model {
                id: IncomingWebhookDeliveryId::now_v7(),
                git_host_connection: *connection,
                ..entry.clone()
            };
            if let Err(e) = gradient_db::incoming_deliveries::record(db, row).await {
                warn!(error = %e, %connection, "Failed to record an inbound webhook delivery.");
            }
        }
    }
}

fn entry(event: Option<&str>, result: &WebResult<IncomingWebhookResponse>) -> Model {
    let now = gradient_types::now();
    match result {
        Err(e) => Model {
            event: truncated_webhook_event_name(event.unwrap_or_default()),
            status_code: i16::try_from(e.status().as_u16()).unwrap_or(i16::MAX),
            result: DeliveryResult::Refused,
            detail: Some(e.public_message()),
            created_at: now,
            ..Default::default()
        },
        Ok(response) => {
            let (result, detail) = outcome(response);
            Model {
                event: truncated_webhook_event_name(&response.event),
                status_code: 200,
                result,
                detail,
                repository: response.repository_urls.first().cloned(),
                tasks_scanned: i32::try_from(response.tasks_scanned).unwrap_or(i32::MAX),
                queued: i32::try_from(response.queued.len()).unwrap_or(i32::MAX),
                created_at: now,
                ..Default::default()
            }
        }
    }
}

fn truncated_webhook_event_name(event: &str) -> String {
    event.chars().take(MAX_EVENT_NAME_LENGTH).collect()
}

fn outcome(response: &IncomingWebhookResponse) -> (DeliveryResult, Option<String>) {
    if !response.queued.is_empty() {
        return (DeliveryResult::Queued, None);
    }
    if !response.skipped.is_empty() {
        let reasons = response
            .skipped
            .iter()
            .map(|s| format!("{}: {}", s.task_name, s.reason_text))
            .collect::<Vec<_>>()
            .join("; ");
        return (DeliveryResult::Skipped, Some(reasons));
    }
    if response.repository_urls.is_empty() {
        return (DeliveryResult::Ignored, None);
    }
    (DeliveryResult::NoMatchingTask, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoints::git_host_hooks::WebhookTriggerOutcome;
    use crate::error::WebError;

    fn response(urls: &[&str], outcome: WebhookTriggerOutcome) -> IncomingWebhookResponse {
        IncomingWebhookResponse::new(
            "push",
            urls.iter().map(|u| u.to_string()).collect(),
            outcome,
        )
    }

    #[test]
    fn a_push_for_an_untracked_repository_reads_as_no_matching_task() {
        let entry = entry(
            None,
            &Ok(response(
                &["https://host/acme/other"],
                WebhookTriggerOutcome::default(),
            )),
        );
        assert_eq!(entry.result, DeliveryResult::NoMatchingTask);
        assert_eq!(entry.repository.as_deref(), Some("https://host/acme/other"));
    }

    #[test]
    fn a_skipped_task_reads_as_text_not_as_a_code() {
        let task = gradient_types::MTask {
            name: "main".into(),
            ..Default::default()
        };
        let outcome = WebhookTriggerOutcome {
            tasks_scanned: 1,
            skipped: vec![crate::endpoints::git_host_hooks::SkippedTask::new(
                &task,
                "acme".into(),
                crate::endpoints::git_host_hooks::TaskEvaluationNotStartedReason::SameCommit,
            )],
            ..Default::default()
        };
        let entry = entry(None, &Ok(response(&["https://host/acme/app"], outcome)));
        assert_eq!(entry.result, DeliveryResult::Skipped);
        assert_eq!(
            entry.detail.as_deref(),
            Some("main: The task already evaluated this commit.")
        );
    }

    #[test]
    fn an_event_without_repository_reads_as_ignored() {
        let entry = entry(None, &Ok(IncomingWebhookResponse::empty("ping")));
        assert_eq!(entry.result, DeliveryResult::Ignored);
        assert_eq!(entry.event, "ping");
    }

    #[test]
    fn a_refusal_keeps_status_and_reason() {
        let refused: WebResult<IncomingWebhookResponse> = Err(WebError::Unauthorized(
            crate::error::ErrorCode::UNAUTHORIZED,
            "invalid webhook signature".into(),
            Vec::new(),
        ));
        let entry = entry(Some("push"), &refused);
        assert_eq!(entry.result, DeliveryResult::Refused);
        assert_eq!(entry.status_code, 401);
        assert_eq!(entry.detail.as_deref(), Some("invalid webhook signature"));
    }

    #[test]
    fn a_long_event_name_is_cut_to_the_limit() {
        let refused: WebResult<IncomingWebhookResponse> = Err(WebError::Unauthorized(
            crate::error::ErrorCode::UNAUTHORIZED,
            "no webhook secret".into(),
            Vec::new(),
        ));
        let entry = entry(Some(&"x".repeat(10_000)), &refused);
        assert_eq!(entry.event.chars().count(), MAX_EVENT_NAME_LENGTH);
    }
}
