/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::fanout::{
    load_trigger_task, parse_trigger_config, project_name_for, push_skipped,
    touch_trigger_last_fired,
};
use super::installation::{event_repo_matches_task, repo_identity};
use super::response::{QueuedEvaluation, TaskEvaluationNotStartedReason, WebhookTriggerOutcome};
use gradient_ci::CiContext;
use gradient_ci::trigger::{InputUpdateOutcome, maybe_trigger_input_update};
use gradient_entity::task_trigger as ept;
use gradient_git_host::ParsedPushEvent;
use gradient_types::triggers::{TriggerConfig, TriggerType};
use gradient_types::*;
use tracing::{info, warn};

pub(super) async fn trigger_input_updates_for_push(
    ci: &CiContext,
    integration_id: IntegrationId,
    push: &ParsedPushEvent,
) -> WebhookTriggerOutcome {
    let mut outcome = WebhookTriggerOutcome::default();
    if push.is_tag {
        return outcome;
    }
    for (trig, task) in input_update_triggers(ci, integration_id, push).await {
        if !task_follows_pushed_input(ci, &task, push).await {
            continue;
        }
        start_input_update(ci, &trig, &task, &mut outcome).await;
    }
    outcome
}

async fn input_update_triggers(
    ci: &CiContext,
    integration_id: IntegrationId,
    push: &ParsedPushEvent,
) -> Vec<(ept::Model, MTask)> {
    let db = &ci.db.web_db;
    let triggers =
        gradient_db::triggers::active_of_integration(db, integration_id, TriggerType::ReporterPush)
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "load triggers for integration");
                Vec::new()
            });

    let mut found = Vec::new();
    for trig in triggers {
        let includes_inputs = matches!(
            parse_trigger_config(&trig),
            Some(TriggerConfig::ReporterPush {
                include_flake_inputs: true,
                ..
            })
        );
        if !includes_inputs {
            continue;
        }
        let Some(task) = load_trigger_task(db, &trig).await else {
            continue;
        };
        if task.active && !event_repo_matches_task(&push.repository_urls, &task.repository) {
            found.push((trig, task));
        }
    }
    found
}

async fn task_follows_pushed_input(ci: &CiContext, task: &MTask, push: &ParsedPushEvent) -> bool {
    match gradient_db::flake_input_sources::of_task(&ci.db.web_db, task.id).await {
        Ok(sources) => sources.iter().any(|source| {
            input_source_matches_push(
                &source.repository_url,
                source.branch.as_deref(),
                &push.repository_urls,
                &push.ref_name,
                push.default_branch.as_deref(),
            )
        }),
        Err(e) => {
            warn!(error = %e, task_id = %task.id, "Load the saved repository for the flake input");
            false
        }
    }
}

async fn start_input_update(
    ci: &CiContext,
    trig: &ept::Model,
    task: &MTask,
    outcome: &mut WebhookTriggerOutcome,
) {
    let project_name = project_name_for(&ci.db.web_db, task.project)
        .await
        .unwrap_or_default();
    outcome.tasks_scanned += 1;

    let Some(base_commit) = task_head(ci, trig, task).await else {
        push_skipped(
            outcome,
            task,
            project_name,
            TaskEvaluationNotStartedReason::Error,
        );
        return;
    };

    let started = maybe_trigger_input_update(&ci.db.web_db, task, base_commit, Some(trig.id)).await;
    touch_trigger_last_fired(&ci.db.web_db, trig).await;
    match started {
        Ok(InputUpdateOutcome::Started(evaluations)) => {
            info!(task_id = %task.id, count = evaluations.len(), "A push of a flake input started an update of the flake input.");
            outcome.queued.extend(
                evaluations
                    .into_iter()
                    .map(|id| QueuedEvaluation::new(task, project_name.clone(), id)),
            );
        }
        Ok(InputUpdateOutcome::AlreadyRunning) => push_skipped(
            outcome,
            task,
            project_name,
            TaskEvaluationNotStartedReason::InputUpdateRunning,
        ),
        Ok(InputUpdateOutcome::NotConfigured) => push_skipped(
            outcome,
            task,
            project_name,
            TaskEvaluationNotStartedReason::InputUpdateNotConfigured,
        ),
        Err(e) => {
            warn!(error = %e, task_id = %task.id, "Start an update of the flake inputs for a push of flake inputs.");
            push_skipped(
                outcome,
                task,
                project_name,
                TaskEvaluationNotStartedReason::Error,
            );
        }
    }
}

async fn task_head(ci: &CiContext, trig: &ept::Model, task: &MTask) -> Option<Vec<u8>> {
    let branch = parse_trigger_config(trig).and_then(|cfg| cfg.trigger_branch().map(str::to_owned));
    match gradient_sources::remote_branch_head(&ci.db, task, branch.as_deref()).await {
        Ok(head) => Some(head.commit),
        Err(e) => {
            warn!(error = %e, task_id = %task.id, "Resolve the HEAD of the task repository for the push of a flake input.");
            None
        }
    }
}

/// Tracked inputs without a branch use the default branch of the repository. A push can match only
/// on that branch.
fn input_source_matches_push(
    source_repository: &str,
    source_branch: Option<&str>,
    event_repo_urls: &[String],
    pushed_branch: &str,
    default_branch: Option<&str>,
) -> bool {
    let Some(source) = repo_identity(source_repository) else {
        return false;
    };
    let same_repository = event_repo_urls
        .iter()
        .any(|url| repo_identity(url).is_some_and(|identity| identity == source));
    let followed_branch = source_branch.or(default_branch);
    same_repository && followed_branch == Some(pushed_branch)
}

#[cfg(test)]
mod tests {
    use super::input_source_matches_push;

    fn fork_urls() -> Vec<String> {
        vec![
            "https://github.com/Dennis/nixpkgs.git".into(),
            "git@github.com:Dennis/nixpkgs.git".into(),
        ]
    }

    #[test]
    fn a_push_to_the_followed_branch_of_the_input_repository_matches() {
        assert!(input_source_matches_push(
            "https://github.com/dennis/nixpkgs",
            Some("nixos-unstable"),
            &fork_urls(),
            "nixos-unstable",
            Some("master"),
        ));
    }

    #[test]
    fn a_push_to_another_branch_does_not_match() {
        assert!(!input_source_matches_push(
            "https://github.com/dennis/nixpkgs",
            Some("nixos-unstable"),
            &fork_urls(),
            "master",
            Some("master"),
        ));
    }

    #[test]
    fn an_input_without_branch_follows_the_default_branch() {
        let source = "https://github.com/dennis/nixpkgs";
        assert!(input_source_matches_push(
            source,
            None,
            &fork_urls(),
            "master",
            Some("master")
        ));
        assert!(!input_source_matches_push(
            source,
            None,
            &fork_urls(),
            "feature",
            Some("master")
        ));
        assert!(!input_source_matches_push(
            source,
            None,
            &fork_urls(),
            "master",
            None
        ));
    }

    #[test]
    fn a_push_to_another_repository_does_not_match() {
        assert!(!input_source_matches_push(
            "https://github.com/NixOS/nixpkgs",
            Some("nixos-unstable"),
            &fork_urls(),
            "nixos-unstable",
            Some("master"),
        ));
    }

    #[test]
    fn an_ssh_input_url_matches_the_https_push() {
        assert!(input_source_matches_push(
            "ssh://git@github.com/dennis/nixpkgs.git",
            Some("nixos-unstable"),
            &fork_urls(),
            "nixos-unstable",
            None,
        ));
    }

    #[test]
    fn a_push_without_repository_urls_never_matches() {
        assert!(!input_source_matches_push(
            "https://github.com/dennis/nixpkgs",
            Some("nixos-unstable"),
            &[],
            "nixos-unstable",
            None,
        ));
    }
}
