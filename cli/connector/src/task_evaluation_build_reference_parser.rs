// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::{Client, ConnectorError};
use std::str::FromStr;

const FULL_COMMIT_HASH_LENGTH: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOrEvaluationReference {
    UuidWithoutContext(String),
    TaskFromProjectTaskAtCommitOrUrl {
        project: String,
        task: String,
        commit: Option<String>,
    },
    ProjectEvaluationLogPage {
        project: String,
        evaluation: String,
        build: Option<String>,
    },
}

impl FromStr for TaskOrEvaluationReference {
    type Err = ConnectorError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        let invalid = || {
            ConnectorError::InvalidTaskEvaluationOrBuildArgument(format!(
                "'{input}' is not a UUID, `project/task`, `project/task@<commit>`, or a URL for a Gradient page."
            ))
        };
        if input.contains("://") || input.starts_with('/') {
            return parse_page_url(input).ok_or_else(invalid);
        }
        if uuid::Uuid::parse_str(input).is_ok() {
            return Ok(Self::UuidWithoutContext(input.to_owned()));
        }
        let (project, task) = input.split_once('/').ok_or_else(invalid)?;
        let (task, commit) = match task.split_once('@') {
            Some((task, commit)) => (task, Some(commit)),
            None => (task, None),
        };
        if [project, task]
            .iter()
            .any(|s| s.is_empty() || s.contains('/'))
            || commit.is_some_and(|c| c.is_empty() || !c.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(invalid());
        }
        Ok(Self::TaskFromProjectTaskAtCommitOrUrl {
            project: project.to_owned(),
            task: task.to_owned(),
            commit: commit.map(str::to_owned),
        })
    }
}

fn parse_page_url(input: &str) -> Option<TaskOrEvaluationReference> {
    let without_scheme = input.split_once("://").map_or(input, |(_, rest)| rest);
    let path_start = without_scheme.find('/')?;
    let (path, query) = match without_scheme[path_start..].split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (&without_scheme[path_start..], None),
    };
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let at = segments.iter().position(|s| *s == "project")?;
    match segments.get(at + 1..at + 4)? {
        [project, "task", task] => Some(
            TaskOrEvaluationReference::TaskFromProjectTaskAtCommitOrUrl {
                project: (*project).to_owned(),
                task: (*task).to_owned(),
                commit: None,
            },
        ),
        [project, "log", evaluation] => {
            let build = query
                .into_iter()
                .flat_map(|q| q.split('&'))
                .find_map(|pair| pair.strip_prefix("build="))
                .filter(|b| !b.is_empty())
                .map(str::to_owned);
            Some(TaskOrEvaluationReference::ProjectEvaluationLogPage {
                project: (*project).to_owned(),
                evaluation: (*evaluation).to_owned(),
                build,
            })
        }
        _ => None,
    }
}

impl TaskOrEvaluationReference {
    pub async fn get_evaluation_uuid(&self, client: &Client) -> Result<String, ConnectorError> {
        match self {
            Self::UuidWithoutContext(id) => Ok(id.clone()),
            Self::ProjectEvaluationLogPage { evaluation, .. } => Ok(evaluation.clone()),
            Self::TaskFromProjectTaskAtCommitOrUrl {
                project,
                task,
                commit: None,
            } => client
                .tasks()
                .evaluations(project, task)
                .await?
                .first()
                .map(|e| e.id.to_string())
                .ok_or_else(|| {
                    ConnectorError::InvalidTaskEvaluationOrBuildArgument(format!(
                        "Task {project}/{task} has no evaluation yet."
                    ))
                }),
            Self::TaskFromProjectTaskAtCommitOrUrl {
                project,
                task,
                commit: Some(commit),
            } => {
                let evaluations = if commit.len() == FULL_COMMIT_HASH_LENGTH {
                    client
                        .tasks()
                        .list_task_evaluations_at_commit(project, task, commit)
                        .await?
                } else {
                    client.tasks().evaluations(project, task).await?
                };
                evaluations
                    .iter()
                    .find(|e| e.commit.starts_with(commit.as_str()))
                    .map(|e| e.id.to_string())
                    .ok_or_else(|| {
                        ConnectorError::InvalidTaskEvaluationOrBuildArgument(format!(
                            "No evaluation for commit {commit} on task {project}/{task}."
                        ))
                    })
            }
        }
    }

    pub fn to_build_uuid(&self) -> Result<String, ConnectorError> {
        match self {
            Self::UuidWithoutContext(id) => Ok(id.clone()),
            Self::ProjectEvaluationLogPage {
                build: Some(build), ..
            } => Ok(build.clone()),
            _ => Err(ConnectorError::InvalidTaskEvaluationOrBuildArgument(
                format!(
                    "'{}' doesn't refer to a build. Pass a build UUID or a log page URL that ends with `?build=<id>`.",
                    self.as_typed()
                ),
            )),
        }
    }

    pub fn task(&self) -> Option<(&str, &str)> {
        match self {
            Self::TaskFromProjectTaskAtCommitOrUrl { project, task, .. } => Some((project, task)),
            _ => None,
        }
    }

    fn as_typed(&self) -> String {
        match self {
            Self::UuidWithoutContext(id) => id.clone(),
            Self::TaskFromProjectTaskAtCommitOrUrl {
                project,
                task,
                commit,
            } => match commit {
                Some(commit) => format!("{project}/{task}@{commit}"),
                None => format!("{project}/{task}"),
            },
            Self::ProjectEvaluationLogPage {
                project,
                evaluation,
                ..
            } => format!("/project/{project}/log/{evaluation}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TaskOrEvaluationReference as Ref;

    fn parse(input: &str) -> Ref {
        input.parse().unwrap_or_else(|e| panic!("{input}: {e}"))
    }

    #[test]
    fn parses_uuids_tasks_commits_and_web_page_urls() {
        let id = "01900000-0000-7000-8000-000000000001";
        assert_eq!(parse(id), Ref::UuidWithoutContext(id.into()));
        assert_eq!(
            parse("infra/master"),
            Ref::TaskFromProjectTaskAtCommitOrUrl {
                project: "infra".into(),
                task: "master".into(),
                commit: None
            }
        );
        assert_eq!(
            parse("infra/master@9c1a2b3"),
            Ref::TaskFromProjectTaskAtCommitOrUrl {
                project: "infra".into(),
                task: "master".into(),
                commit: Some("9c1a2b3".into())
            }
        );
        assert_eq!(
            parse("https://ci.example/project/infra/task/master/settings"),
            parse("infra/master")
        );
        assert_eq!(
            parse(&format!(
                "https://ci.example/project/infra/log/{id}?tab=x&build=b1"
            )),
            Ref::ProjectEvaluationLogPage {
                project: "infra".into(),
                evaluation: id.into(),
                build: Some("b1".into())
            }
        );
        assert_eq!(
            parse(&format!("/project/infra/log/{id}"))
                .to_build_uuid()
                .ok(),
            None
        );
    }

    #[test]
    fn rejects_names_without_project_and_urls_of_other_pages() {
        for input in [
            "master",
            "infra/",
            "/infra",
            "infra/master@",
            "infra/master@not-a-hash",
            "a/b/c",
            "https://ci.example/caches/main",
        ] {
            assert!(input.parse::<Ref>().is_err(), "{input} parsed");
        }
    }

    #[test]
    fn a_build_needs_a_uuid_or_a_log_url_with_a_build() {
        let id = "01900000-0000-7000-8000-000000000001";
        assert_eq!(parse(id).to_build_uuid().unwrap(), id);
        assert_eq!(
            parse("https://ci.example/project/p/log/e?build=b")
                .to_build_uuid()
                .unwrap(),
            "b"
        );
        assert!(parse("p/t").to_build_uuid().is_err());
    }
}
