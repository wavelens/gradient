/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Each step carries at most one action. Actions the caller may not run are left out.

use super::RepositoryFetchStatus;
use super::repository::TaskCommitStatusPostAttempt;
use crate::endpoints::git_host_connections::{
    GitHostConnectionSummary, IncomingWebhookDeliveryResponse, url_host,
};
use gradient_entity::incoming_webhook_delivery::DeliveryResult;
use gradient_types::GitHostType;
use serde::Serialize;

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskGitHostChainStepState {
    Ok,
    Attention,
    Failing,
    Off,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskGitHostChainStepAction {
    EditRepositoryUrl,
    SelectConnection,
    CreateConnection {
        git_host_type: Option<String>,
        base_url: String,
    },
    InstallGithubApp {
        account: String,
    },
    InstallWebhook,
    AddWebhookOnHost {
        webhook_url: String,
    },
    TurnOnReportStatus,
    TestStatusReport,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq, utoipa::ToSchema)]
pub struct TaskGitHostChainStep {
    pub state: TaskGitHostChainStepState,
    pub detail: String,
    pub at: Option<chrono::NaiveDateTime>,
    pub action: Option<TaskGitHostChainStepAction>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq, utoipa::ToSchema)]
pub struct TaskGitHostChain {
    pub repository: TaskGitHostChainStep,
    pub connection: TaskGitHostChainStep,
    pub incoming_events: TaskGitHostChainStep,
    pub commit_status: TaskGitHostChainStep,
}

pub(crate) struct TaskGitHostChainInput<'a> {
    pub(crate) repository: &'a str,
    pub(crate) fetch: Option<&'a RepositoryFetchStatus>,
    pub(crate) connection: Option<&'a GitHostConnectionSummary>,
    pub(crate) connection_has_access_token: bool,
    pub(crate) available_connections: &'a [GitHostConnectionSummary],
    pub(crate) report_status: bool,
    pub(crate) newest_delivery: Option<&'a IncomingWebhookDeliveryResponse>,
    pub(crate) newest_status_post: Option<&'a TaskCommitStatusPostAttempt>,
    pub(crate) can_edit_task: bool,
    pub(crate) can_manage_integrations: bool,
    pub(crate) github_app_configured: bool,
    pub(crate) can_install_github_app: bool,
}

struct RepositoryOnHost {
    host: String,
    owner: String,
    name: String,
}

fn step(
    state: TaskGitHostChainStepState,
    detail: impl Into<String>,
    action: Option<TaskGitHostChainStepAction>,
) -> TaskGitHostChainStep {
    TaskGitHostChainStep {
        state,
        detail: detail.into(),
        at: None,
        action,
    }
}

fn off(detail: &str) -> TaskGitHostChainStep {
    step(TaskGitHostChainStepState::Off, detail, None)
}

fn read_repository(repository: &str) -> Option<RepositoryOnHost> {
    let lower = repository.trim().to_ascii_lowercase();
    let lower = lower.strip_prefix("git+").unwrap_or(&lower);
    let host = if lower.starts_with("github:") {
        "github.com".to_owned()
    } else if lower.starts_with("gitlab:") {
        "gitlab.com".to_owned()
    } else {
        url_host(repository)?
    };
    let (owner, name) = gradient_ci::parse_owner_repo(repository)?;
    Some(RepositoryOnHost { host, owner, name })
}

fn guess_git_host_type(host: &str) -> Option<GitHostType> {
    match host {
        "github.com" => Some(GitHostType::GitHub),
        "gitlab.com" => Some(GitHostType::GitLab),
        "codeberg.org" => Some(GitHostType::Forgejo),
        "gitea.com" => Some(GitHostType::Gitea),
        _ => None,
    }
}

fn is_github_app_installation(connection: &GitHostConnectionSummary) -> bool {
    connection.git_host_type == GitHostType::GitHub.as_path_segment()
        && connection.account_login.is_some()
}

fn connection_label(connection: &GitHostConnectionSummary) -> String {
    match &connection.account_login {
        Some(account) => format!("{} ({account})", connection.display_name),
        None => connection.display_name.clone(),
    }
}

fn repository_step(
    input: &TaskGitHostChainInput<'_>,
    repository: Option<&RepositoryOnHost>,
) -> TaskGitHostChainStep {
    let edit = input
        .can_edit_task
        .then_some(TaskGitHostChainStepAction::EditRepositoryUrl);
    match repository {
        Some(repository) => match input.fetch.filter(|fetch| fetch.failed) {
            Some(fetch) => TaskGitHostChainStep {
                at: Some(fetch.checked_at),
                ..step(
                    TaskGitHostChainStepState::Failing,
                    match &fetch.error {
                        Some(error) => format!("Can't fetch the repository: {error}"),
                        None => format!(
                            "Can't fetch {}/{} from {}",
                            repository.owner, repository.name, repository.host
                        ),
                    },
                    edit,
                )
            },
            None => step(
                TaskGitHostChainStepState::Ok,
                format!(
                    "{}/{} on {}",
                    repository.owner, repository.name, repository.host
                ),
                None,
            ),
        },
        None => step(
            TaskGitHostChainStepState::Failing,
            format!(
                "Can't read host, owner and repository from {}",
                input.repository
            ),
            edit,
        ),
    }
}

fn connection_step(
    input: &TaskGitHostChainInput<'_>,
    repository: &RepositoryOnHost,
) -> TaskGitHostChainStep {
    let select = input
        .can_edit_task
        .then_some(TaskGitHostChainStepAction::SelectConnection);
    if let Some(connection) = input.connection {
        return match connection.repository_fit.as_ref().filter(|fit| !fit.fits) {
            Some(misfit) => step(
                TaskGitHostChainStepState::Failing,
                misfit.reason.clone().unwrap_or_default(),
                select,
            ),
            None => step(
                TaskGitHostChainStepState::Ok,
                connection_label(connection),
                None,
            ),
        };
    }
    let fitting: Vec<&GitHostConnectionSummary> = input
        .available_connections
        .iter()
        .filter(|c| c.repository_fit.as_ref().is_some_and(|fit| fit.fits))
        .collect();
    match fitting.as_slice() {
        [only] => step(
            TaskGitHostChainStepState::Attention,
            format!("{} fits but isn't selected", connection_label(only)),
            select,
        ),
        [_, ..] => step(
            TaskGitHostChainStepState::Attention,
            format!("{} connections fit; none is selected", fitting.len()),
            select,
        ),
        [] if repository.host == "github.com" && !input.github_app_configured => step(
            TaskGitHostChainStepState::Attention,
            "This server has no GitHub App. An instance admin can set one up.",
            None,
        ),
        [] if repository.host == "github.com" && input.can_install_github_app => step(
            TaskGitHostChainStepState::Attention,
            format!("The GitHub App isn't installed on {}", repository.owner),
            Some(TaskGitHostChainStepAction::InstallGithubApp {
                account: repository.owner.clone(),
            }),
        ),
        [] if repository.host == "github.com" => step(
            TaskGitHostChainStepState::Attention,
            format!(
                "The GitHub App isn't installed on {}. An instance admin can install it.",
                repository.owner
            ),
            None,
        ),
        [] => step(
            TaskGitHostChainStepState::Attention,
            format!("No connection to {}", repository.host),
            input
                .can_manage_integrations
                .then(|| TaskGitHostChainStepAction::CreateConnection {
                    git_host_type: guess_git_host_type(&repository.host)
                        .map(|t| t.as_path_segment().to_owned()),
                    base_url: format!("https://{}", repository.host),
                }),
        ),
    }
}

fn delivery_state(delivery: &IncomingWebhookDeliveryResponse) -> TaskGitHostChainStepState {
    if delivery.status_code >= 400 || delivery.result == DeliveryResult::Refused {
        TaskGitHostChainStepState::Failing
    } else if delivery.result == DeliveryResult::NoMatchingTask {
        TaskGitHostChainStepState::Attention
    } else {
        TaskGitHostChainStepState::Ok
    }
}

fn delivery_outcome(delivery: &IncomingWebhookDeliveryResponse) -> String {
    let result = match delivery.result {
        DeliveryResult::Queued => "queued an evaluation",
        DeliveryResult::Skipped => "skipped",
        DeliveryResult::NoMatchingTask => "matched no task",
        DeliveryResult::Ignored => "ignored",
        DeliveryResult::Refused => "refused",
    };
    match &delivery.detail {
        Some(detail) => format!("{} event {result}: {detail}", delivery.event),
        None => format!("{} event {result}", delivery.event),
    }
}

fn webhook_action(
    input: &TaskGitHostChainInput<'_>,
    connection: &GitHostConnectionSummary,
) -> Option<TaskGitHostChainStepAction> {
    if input.can_manage_integrations && input.connection_has_access_token {
        Some(TaskGitHostChainStepAction::InstallWebhook)
    } else {
        connection
            .webhook_url
            .clone()
            .map(|webhook_url| TaskGitHostChainStepAction::AddWebhookOnHost { webhook_url })
    }
}

fn incoming_events_step(
    input: &TaskGitHostChainInput<'_>,
    connection: &GitHostConnectionSummary,
) -> TaskGitHostChainStep {
    if !connection.receives_events {
        return off("The connection doesn't receive events");
    }
    let github_app = is_github_app_installation(connection);
    let refused_after_last_delivery = connection.last_refused_signature_at.filter(|refused| {
        connection
            .last_delivery
            .as_ref()
            .is_none_or(|delivery| delivery.created_at < *refused)
    });
    if let Some(refused_at) = refused_after_last_delivery {
        return TaskGitHostChainStep {
            at: Some(refused_at),
            ..step(
                TaskGitHostChainStepState::Failing,
                "The last delivery had a wrong signature; the webhook secrets differ",
                (!github_app)
                    .then(|| webhook_action(input, connection))
                    .flatten(),
            )
        };
    }
    match input.newest_delivery {
        Some(delivery) => TaskGitHostChainStep {
            at: Some(delivery.created_at),
            ..step(delivery_state(delivery), delivery_outcome(delivery), None)
        },
        None if github_app => step(
            TaskGitHostChainStepState::Attention,
            "No event from GitHub yet",
            None,
        ),
        None => step(
            TaskGitHostChainStepState::Attention,
            "No event received yet",
            webhook_action(input, connection),
        ),
    }
}

fn commit_status_step(
    input: &TaskGitHostChainInput<'_>,
    connection: &GitHostConnectionSummary,
) -> TaskGitHostChainStep {
    if !connection.reports_status {
        return off("The connection doesn't report commit status");
    }
    if !input.report_status {
        return step(
            TaskGitHostChainStepState::Off,
            "Off for this task",
            input
                .can_edit_task
                .then_some(TaskGitHostChainStepAction::TurnOnReportStatus),
        );
    }
    let test = input
        .can_edit_task
        .then_some(TaskGitHostChainStepAction::TestStatusReport);
    match input.newest_status_post {
        Some(post) if post.success => TaskGitHostChainStep {
            at: Some(post.delivered_at),
            ..step(
                TaskGitHostChainStepState::Ok,
                format!("Last {} status posted", post.event),
                None,
            )
        },
        Some(post) => TaskGitHostChainStep {
            at: Some(post.delivered_at),
            ..step(
                TaskGitHostChainStepState::Failing,
                match (&post.error_message, post.response_status) {
                    (Some(error), _) => format!("Last status post failed: {error}"),
                    (None, Some(status)) => {
                        format!("Last status post failed with HTTP {status}")
                    }
                    (None, None) => "Last status post failed".to_owned(),
                },
                test,
            )
        },
        None => step(
            TaskGitHostChainStepState::Attention,
            "No status posted yet",
            test,
        ),
    }
}

pub(crate) fn task_git_host_chain(input: &TaskGitHostChainInput<'_>) -> TaskGitHostChain {
    let repository = read_repository(input.repository);
    let repository_step = repository_step(input, repository.as_ref());
    let Some(repository) = repository else {
        return TaskGitHostChain {
            repository: repository_step,
            connection: off("Needs a readable repository URL"),
            incoming_events: off("Needs a connection"),
            commit_status: off("Needs a connection"),
        };
    };
    let connection_step = connection_step(input, &repository);
    let usable_connection = input
        .connection
        .filter(|_| connection_step.state == TaskGitHostChainStepState::Ok);
    let (incoming_events, commit_status) = match usable_connection {
        Some(connection) => (
            incoming_events_step(input, connection),
            commit_status_step(input, connection),
        ),
        None => (off("Needs a connection"), off("Needs a connection")),
    };
    TaskGitHostChain {
        repository: repository_step,
        connection: connection_step,
        incoming_events,
        commit_status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoints::git_host_connections::RepositoryFit;
    use TaskGitHostChainStepState::*;
    use gradient_fixtures::test_date;
    use gradient_types::git_host::GitHostConnectionOwner;
    use gradient_types::ids::{GitHostConnectionId, IncomingWebhookDeliveryId};

    const GITEA_REPOSITORY: &str = "https://git.example.com/acme/app.git";

    fn connection(fits: bool) -> GitHostConnectionSummary {
        GitHostConnectionSummary {
            id: GitHostConnectionId::nil(),
            owner: GitHostConnectionOwner::Instance,
            name: "gitea".to_owned(),
            display_name: "Gitea".to_owned(),
            git_host_type: "gitea".to_owned(),
            base_url: Some("https://git.example.com".to_owned()),
            receives_events: true,
            reports_status: true,
            can_manage: true,
            account_login: None,
            webhook_url: Some("https://ci.example.com/api/v1/hooks/gitea/gitea".to_owned()),
            access_token_expires_at: None,
            access_token_refreshes_automatically: false,
            last_delivery: None,
            refused_signature_count: 0,
            last_refused_signature_at: None,
            last_status_post: None,
            task_count: 0,
            repository_fit: Some(RepositoryFit {
                fits,
                reason: (!fits).then(|| "Points at another host".to_owned()),
            }),
        }
    }

    fn delivery() -> IncomingWebhookDeliveryResponse {
        IncomingWebhookDeliveryResponse {
            id: IncomingWebhookDeliveryId::nil(),
            event: "push".to_owned(),
            status_code: 200,
            result: DeliveryResult::Queued,
            detail: None,
            repository: Some("acme/app".to_owned()),
            tasks_scanned: 1,
            queued: 1,
            created_at: test_date(),
        }
    }

    fn input<'a>(
        repository: &'a str,
        connection: Option<&'a GitHostConnectionSummary>,
        available_connections: &'a [GitHostConnectionSummary],
    ) -> TaskGitHostChainInput<'a> {
        TaskGitHostChainInput {
            repository,
            fetch: None,
            connection,
            connection_has_access_token: true,
            available_connections,
            report_status: true,
            newest_delivery: None,
            newest_status_post: None,
            can_edit_task: true,
            can_manage_integrations: true,
            github_app_configured: true,
            can_install_github_app: true,
        }
    }

    #[test]
    fn unreadable_repository_fails_and_turns_off_the_rest() {
        let chain = task_git_host_chain(&input("not a url", None, &[]));
        assert_eq!(chain.repository.state, Failing);
        assert_eq!(
            chain.repository.action,
            Some(TaskGitHostChainStepAction::EditRepositoryUrl)
        );
        assert_eq!(chain.connection.state, Off);
        assert_eq!(chain.incoming_events.state, Off);
        assert_eq!(chain.commit_status.state, Off);
    }

    #[test]
    fn single_fitting_connection_asks_to_be_selected() {
        let available = [connection(true), connection(false)];
        let chain = task_git_host_chain(&input(GITEA_REPOSITORY, None, &available));
        assert_eq!(chain.repository.state, Ok);
        assert_eq!(chain.connection.state, Attention);
        assert_eq!(
            chain.connection.action,
            Some(TaskGitHostChainStepAction::SelectConnection)
        );
        assert_eq!(chain.incoming_events.state, Off);
    }

    #[test]
    fn github_repository_without_installation_asks_to_install_the_app() {
        let chain = task_git_host_chain(&input("github:acme/app", None, &[]));
        assert_eq!(chain.connection.state, Attention);
        assert_eq!(
            chain.connection.action,
            Some(TaskGitHostChainStepAction::InstallGithubApp {
                account: "acme".to_owned()
            })
        );
    }

    #[test]
    fn github_repository_without_installation_names_the_instance_admin_when_the_caller_may_not_install()
     {
        let chain = task_git_host_chain(&TaskGitHostChainInput {
            can_install_github_app: false,
            ..input("github:acme/app", None, &[])
        });
        assert_eq!(chain.connection.state, Attention);
        assert_eq!(chain.connection.action, None);
        assert!(
            chain
                .connection
                .detail
                .contains("instance admin can install")
        );
    }

    #[test]
    fn github_repository_on_a_server_without_github_app_offers_no_install() {
        let chain = task_git_host_chain(&TaskGitHostChainInput {
            github_app_configured: false,
            ..input("github:acme/app", None, &[])
        });
        assert_eq!(chain.connection.state, Attention);
        assert_eq!(chain.connection.action, None);
        assert!(chain.connection.detail.contains("no GitHub App"));
    }

    #[test]
    fn unknown_host_without_connection_offers_to_create_one() {
        let chain = task_git_host_chain(&input(GITEA_REPOSITORY, None, &[]));
        assert_eq!(
            chain.connection.action,
            Some(TaskGitHostChainStepAction::CreateConnection {
                git_host_type: None,
                base_url: "https://git.example.com".to_owned(),
            })
        );
    }

    #[test]
    fn misfitting_selected_connection_fails() {
        let selected = connection(false);
        let chain = task_git_host_chain(&input(GITEA_REPOSITORY, Some(&selected), &[]));
        assert_eq!(chain.connection.state, Failing);
        assert_eq!(chain.connection.detail, "Points at another host");
        assert_eq!(chain.incoming_events.state, Off);
    }

    #[test]
    fn report_status_off_offers_to_turn_it_on() {
        let selected = connection(true);
        let chain = task_git_host_chain(&TaskGitHostChainInput {
            report_status: false,
            ..input(GITEA_REPOSITORY, Some(&selected), &[])
        });
        assert_eq!(chain.commit_status.state, Off);
        assert_eq!(
            chain.commit_status.action,
            Some(TaskGitHostChainStepAction::TurnOnReportStatus)
        );
    }

    #[test]
    fn refused_signature_after_last_delivery_fails() {
        let mut selected = connection(true);
        selected.last_delivery = Some(delivery());
        selected.last_refused_signature_at = Some(test_date() + chrono::Duration::minutes(1));
        let newest = delivery();
        let chain = task_git_host_chain(&TaskGitHostChainInput {
            newest_delivery: Some(&newest),
            ..input(GITEA_REPOSITORY, Some(&selected), &[])
        });
        assert_eq!(chain.incoming_events.state, Failing);
        assert_eq!(chain.incoming_events.at, selected.last_refused_signature_at);
        assert_eq!(
            chain.incoming_events.action,
            Some(TaskGitHostChainStepAction::InstallWebhook)
        );
    }

    #[test]
    fn delivery_after_refused_signature_is_ok() {
        let mut selected = connection(true);
        selected.last_refused_signature_at = Some(test_date() - chrono::Duration::minutes(1));
        selected.last_delivery = Some(delivery());
        let newest = delivery();
        let chain = task_git_host_chain(&TaskGitHostChainInput {
            newest_delivery: Some(&newest),
            ..input(GITEA_REPOSITORY, Some(&selected), &[])
        });
        assert_eq!(chain.incoming_events.state, Ok);
        assert_eq!(
            chain.incoming_events.detail,
            "push event queued an evaluation"
        );
    }

    fn failed_fetch(error: Option<&str>) -> RepositoryFetchStatus {
        RepositoryFetchStatus {
            checked_at: test_date(),
            ok_at: None,
            failed: true,
            error: error.map(str::to_owned),
        }
    }

    #[test]
    fn failed_fetch_fails_the_repository_step_with_its_error() {
        let fetch = failed_fetch(Some("Name or service not known"));
        let chain = task_git_host_chain(&TaskGitHostChainInput {
            fetch: Some(&fetch),
            ..input(GITEA_REPOSITORY, None, &[])
        });
        assert_eq!(chain.repository.state, Failing);
        assert_eq!(
            chain.repository.detail,
            "Can't fetch the repository: Name or service not known"
        );
        assert_eq!(chain.repository.at, Some(test_date()));
        assert_eq!(
            chain.repository.action,
            Some(TaskGitHostChainStepAction::EditRepositoryUrl)
        );
    }

    #[test]
    fn failed_fetch_with_hidden_error_names_repository_and_host() {
        let fetch = failed_fetch(None);
        let chain = task_git_host_chain(&TaskGitHostChainInput {
            fetch: Some(&fetch),
            ..input(GITEA_REPOSITORY, None, &[])
        });
        assert_eq!(
            chain.repository.detail,
            "Can't fetch acme/app from git.example.com"
        );
    }

    #[test]
    fn successful_fetch_keeps_the_repository_step_ok() {
        let fetch = RepositoryFetchStatus {
            ok_at: Some(test_date()),
            failed: false,
            ..failed_fetch(None)
        };
        let chain = task_git_host_chain(&TaskGitHostChainInput {
            fetch: Some(&fetch),
            ..input(GITEA_REPOSITORY, None, &[])
        });
        assert_eq!(chain.repository.state, Ok);
    }

    #[test]
    fn delivery_that_matched_no_task_needs_attention_with_its_reason() {
        let selected = connection(true);
        let newest = IncomingWebhookDeliveryResponse {
            result: DeliveryResult::NoMatchingTask,
            detail: Some("app has no trigger for push events".to_owned()),
            queued: 0,
            ..delivery()
        };
        let chain = task_git_host_chain(&TaskGitHostChainInput {
            newest_delivery: Some(&newest),
            ..input(GITEA_REPOSITORY, Some(&selected), &[])
        });
        assert_eq!(chain.incoming_events.state, Attention);
        assert_eq!(
            chain.incoming_events.detail,
            "push event matched no task: app has no trigger for push events"
        );
    }

    #[test]
    fn view_only_caller_gets_no_edit_actions() {
        fn view_only<'a>(
            repository: &'a str,
            connection: Option<&'a GitHostConnectionSummary>,
            available: &'a [GitHostConnectionSummary],
        ) -> TaskGitHostChainInput<'a> {
            TaskGitHostChainInput {
                can_edit_task: false,
                can_manage_integrations: false,
                report_status: false,
                can_install_github_app: false,
                ..input(repository, connection, available)
            }
        }
        let available = [connection(true)];
        let chain = task_git_host_chain(&view_only("not a url", None, &available));
        assert_eq!(chain.repository.action, None);
        let chain = task_git_host_chain(&view_only(GITEA_REPOSITORY, None, &available));
        assert_eq!(chain.connection.action, None);
        let chain = task_git_host_chain(&view_only("github:acme/app", None, &[]));
        assert_eq!(chain.connection.action, None);
        let selected = connection(true);
        let chain = task_git_host_chain(&view_only(GITEA_REPOSITORY, Some(&selected), &[]));
        assert_eq!(chain.commit_status.action, None);
        assert_eq!(
            chain.incoming_events.action,
            Some(TaskGitHostChainStepAction::AddWebhookOnHost {
                webhook_url: "https://ci.example.com/api/v1/hooks/gitea/gitea".to_owned()
            })
        );
    }
}
