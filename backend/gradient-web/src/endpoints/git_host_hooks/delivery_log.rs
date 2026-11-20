/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::IncomingWebhookResponse;
use super::installation::repo_identity;
use crate::error::WebResult;
use gradient_db::WebDb;
use gradient_entity::incoming_webhook_delivery::{DeliveryResult, Model};
use gradient_types::{
    EGitHostConnection, GitHostConnectionId, IncomingWebhookDeliveryId, MTask, ProjectId,
};
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
        let tracking_tasks = match (&entry.result, &entry.repository) {
            (DeliveryResult::NoMatchingTask, Some(repository)) => {
                tasks_tracking_repository(db, repository).await
            }
            _ => Vec::new(),
        };
        for connection in &self.connections {
            let detail = match (&entry.result, entry.repository.as_deref()) {
                (DeliveryResult::NoMatchingTask, Some(repository)) => {
                    no_matching_task_detail(
                        db,
                        *connection,
                        repository,
                        &entry.event,
                        &tracking_tasks,
                    )
                    .await
                }
                _ => entry.detail.clone(),
            };
            let row = Model {
                id: IncomingWebhookDeliveryId::now_v7(),
                git_host_connection: *connection,
                detail,
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

async fn tasks_tracking_repository(db: &WebDb, repository: &str) -> Vec<MTask> {
    let (Some(identity), Some((_, name))) = (
        repo_identity(repository),
        gradient_ci::parse_owner_repo(repository),
    ) else {
        return Vec::new();
    };
    match gradient_db::tasks::with_repository_containing(db, &name).await {
        Ok(tasks) => tasks
            .into_iter()
            .filter(|task| repo_identity(&task.repository).as_ref() == Some(&identity))
            .collect(),
        Err(e) => {
            warn!(error = %e, "Failed to look up the tasks that track a repository.");
            Vec::new()
        }
    }
}

async fn no_matching_task_detail(
    db: &WebDb,
    connection: GitHostConnectionId,
    repository: &str,
    event: &str,
    tracking_tasks: &[MTask],
) -> Option<String> {
    let connection_project = gradient_db::lookup::by_id::<EGitHostConnection, _>(db, connection)
        .await
        .ok()
        .flatten()
        .and_then(|row| row.project);
    let identity = repo_identity(repository).unwrap_or_else(|| repository.to_owned());
    Some(describe_tasks_tracking_repository(
        &identity,
        event,
        connection,
        connection_project,
        tracking_tasks,
    ))
}

fn task_list_visible_to_connection(
    tasks: &[&MTask],
    connection_project: Option<ProjectId>,
) -> String {
    let (own, other): (Vec<&&MTask>, Vec<&&MTask>) = tasks
        .iter()
        .partition(|task| Some(task.project) == connection_project);
    let mut labels: Vec<String> = own.iter().map(|task| task.name.clone()).collect();
    match other.len() {
        0 => {}
        1 => labels.push("1 task of another project".to_owned()),
        n => labels.push(format!("{n} tasks of other projects")),
    }
    labels.join(", ")
}

fn describe_tasks_tracking_repository(
    identity: &str,
    event: &str,
    connection: GitHostConnectionId,
    connection_project: Option<ProjectId>,
    tracking_tasks: &[MTask],
) -> String {
    if tracking_tasks.is_empty() {
        return format!("No task tracks {identity}");
    }
    let (using, not_using): (Vec<&MTask>, Vec<&MTask>) = tracking_tasks
        .iter()
        .partition(|task| task.git_host_connection == Some(connection));
    if using.is_empty() {
        let (track, uses) = if not_using.len() == 1 {
            ("tracks", "uses")
        } else {
            ("track", "use")
        };
        format!(
            "{} {track} {identity} but {uses} no or another connection",
            task_list_visible_to_connection(&not_using, connection_project)
        )
    } else {
        let (uses, has) = if using.len() == 1 {
            ("uses", "has")
        } else {
            ("use", "have")
        };
        format!(
            "{} {uses} this connection for {identity} but {has} no trigger for {event} events",
            task_list_visible_to_connection(&using, connection_project)
        )
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

    fn tracking_task(
        name: &str,
        project: ProjectId,
        connection: Option<GitHostConnectionId>,
    ) -> MTask {
        MTask {
            name: name.into(),
            project,
            repository: "https://host/acme/app".into(),
            git_host_connection: connection,
            ..Default::default()
        }
    }

    #[test]
    fn no_tracking_task_names_the_repository() {
        let detail = describe_tasks_tracking_repository(
            "acme/app",
            "push",
            GitHostConnectionId::nil(),
            None,
            &[],
        );
        assert_eq!(detail, "No task tracks acme/app");
    }

    #[test]
    fn tasks_on_another_connection_are_named_in_the_connection_project_only() {
        let own = ProjectId::now_v7();
        let connection = GitHostConnectionId::now_v7();
        let tasks = [
            tracking_task("main", own, None),
            tracking_task(
                "other",
                ProjectId::now_v7(),
                Some(GitHostConnectionId::now_v7()),
            ),
        ];
        let detail =
            describe_tasks_tracking_repository("acme/app", "push", connection, Some(own), &tasks);
        assert_eq!(
            detail,
            "main, 1 task of another project track acme/app but use no or another connection"
        );
    }

    #[test]
    fn linked_tasks_without_matching_trigger_name_the_event() {
        let own = ProjectId::now_v7();
        let connection = GitHostConnectionId::now_v7();
        let tasks = [tracking_task("main", own, Some(connection))];
        let detail = describe_tasks_tracking_repository(
            "acme/app",
            "pull_request",
            connection,
            Some(own),
            &tasks,
        );
        assert_eq!(
            detail,
            "main uses this connection for acme/app but has no trigger for pull_request events"
        );
    }

    #[test]
    fn several_linked_tasks_read_in_plural() {
        let own = ProjectId::now_v7();
        let connection = GitHostConnectionId::now_v7();
        let tasks = [
            tracking_task("main", own, Some(connection)),
            tracking_task("docs", own, Some(connection)),
        ];
        let detail =
            describe_tasks_tracking_repository("acme/app", "push", connection, Some(own), &tasks);
        assert_eq!(
            detail,
            "main, docs use this connection for acme/app but have no trigger for push events"
        );
    }

    #[test]
    fn one_task_on_another_connection_reads_in_singular() {
        let own = ProjectId::now_v7();
        let tasks = [tracking_task("main", own, None)];
        let detail = describe_tasks_tracking_repository(
            "acme/app",
            "push",
            GitHostConnectionId::now_v7(),
            Some(own),
            &tasks,
        );
        assert_eq!(
            detail,
            "main tracks acme/app but uses no or another connection"
        );
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
