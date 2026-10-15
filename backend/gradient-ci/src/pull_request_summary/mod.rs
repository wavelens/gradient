/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod load;
mod render;

use crate::actions::evaluation_rows::load_evaluation_rows;
use crate::actions::reporter_for_task;
use crate::context::CiContext;
use crate::{ActionError, parse_owner_repo};
use anyhow::{Context, Result};
use gradient_db::evaluations::branch::newest_on_branch;
use gradient_db::lookup::by_id;
use gradient_db::pull_request_summaries;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_git_host::reporter::CiReporter;
use gradient_types::branch_key;
use gradient_types::*;
use tracing::warn;

const fn summarized(status: EvaluationStatus) -> bool {
    matches!(
        status,
        EvaluationStatus::Completed | EvaluationStatus::Failed
    )
}

pub async fn enable_pull_request_summary(
    ctx: &CiContext,
    task: TaskId,
    owner: &str,
    repo: &str,
    pr_number: u64,
) -> Result<bool> {
    let db = &ctx.db.worker_db;
    let repository = by_id::<ETask, _>(db, task)
        .await?
        .and_then(|t| parse_owner_repo(&t.repository));
    if !repository.is_some_and(|(o, r)| same_repository(&format!("{o}/{r}"), owner, repo)) {
        return Ok(false);
    }

    pull_request_summaries::enable(db, task, pr_number)
        .await
        .context("enable the pull request summary")?;
    if let Some(evaluation) =
        newest_on_branch(db, task, &branch_key::pull_request(pr_number)).await?
        && summarized(evaluation.status)
    {
        publish(ctx, task, pr_number, &evaluation).await?;
    }

    Ok(true)
}

fn same_repository(task_path: &str, owner: &str, repo: &str) -> bool {
    task_path.eq_ignore_ascii_case(&format!("{owner}/{repo}"))
}

pub async fn update_pull_request_summary_on_terminal(
    ctx: &CiContext,
    task: TaskId,
    evaluation: &MEvaluation,
    status: EvaluationStatus,
) {
    if !summarized(status) {
        return;
    }
    let Some(pr_number) = branch_key::pull_request_number(&evaluation.branch) else {
        return;
    };

    let newest = newest_on_branch(&ctx.db.worker_db, task, &evaluation.branch).await;
    let published = match newest {
        Ok(Some(newest)) if newest.id == evaluation.id => {
            publish(ctx, task, pr_number, evaluation).await
        }
        Ok(_) => Ok(()),
        Err(e) => Err(e.into()),
    };
    if let Err(e) = published {
        warn!(error = %e, evaluation_id = %evaluation.id, "updating the pull request summary");
    }
}

async fn publish(
    ctx: &CiContext,
    task: TaskId,
    pr_number: u64,
    evaluation: &MEvaluation,
) -> Result<()> {
    let db = &ctx.db.worker_db;
    let Some(row) = pull_request_summaries::of_pull_request(db, task, pr_number).await? else {
        return Ok(());
    };
    let Some(reporter) = reporter_for_task(ctx, task).await? else {
        return Ok(());
    };

    let rows = load_evaluation_rows(ctx, evaluation).await?;
    let (owner, repo) = parse_owner_repo(&rows.task.repository).ok_or_else(|| {
        ActionError::RepositoryUnparsable {
            repository: rows.task.repository.clone(),
        }
    })?;
    let body = load::load_summary(ctx, evaluation, &rows).await?.render();
    let comment = PullRequestComment {
        reporter: reporter.as_ref(),
        owner: &owner,
        repo: &repo,
        pr_number,
    };
    if let Some(comment_id) = comment.post_or_edit(row.comment_id, &body).await? {
        pull_request_summaries::set_comment(db, row.id, comment_id).await?;
    }

    Ok(())
}

struct PullRequestComment<'a> {
    reporter: &'a dyn CiReporter,
    owner: &'a str,
    repo: &'a str,
    pr_number: u64,
}

impl PullRequestComment<'_> {
    async fn post_or_edit(&self, comment_id: Option<i64>, body: &str) -> Result<Option<i64>> {
        if let Some(id) = comment_id
            && self
                .reporter
                .edit_pr_comment(self.owner, self.repo, self.pr_number, id, body)
                .await?
        {
            return Ok(None);
        }

        self.reporter
            .post_pr_comment(self.owner, self.repo, self.pr_number, body)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use gradient_git_host::reporter::CiReport;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct FakeHost {
        comments: Mutex<Vec<(i64, String)>>,
    }

    #[async_trait]
    impl CiReporter for FakeHost {
        async fn report(&self, _report: &CiReport) -> Result<Option<i64>> {
            Ok(None)
        }

        async fn post_pr_comment(
            &self,
            _: &str,
            _: &str,
            _: u64,
            body: &str,
        ) -> Result<Option<i64>> {
            let mut comments = self.comments.lock().unwrap();
            let id = 100 + comments.len() as i64;
            comments.push((id, body.to_owned()));
            Ok(Some(id))
        }

        async fn edit_pr_comment(
            &self,
            _: &str,
            _: &str,
            _: u64,
            id: i64,
            body: &str,
        ) -> Result<bool> {
            let mut comments = self.comments.lock().unwrap();
            let Some(comment) = comments.iter_mut().find(|(c, _)| *c == id) else {
                return Ok(false);
            };
            comment.1 = body.to_owned();
            Ok(true)
        }
    }

    fn comment(host: &FakeHost) -> PullRequestComment<'_> {
        PullRequestComment {
            reporter: host,
            owner: "acme",
            repo: "widgets",
            pr_number: 7,
        }
    }

    #[test]
    fn a_summary_command_only_enrolls_tasks_of_the_commented_repository() {
        let (owner, repo) = parse_owner_repo("git@gitlab.example.com:group/sub/repo.git").unwrap();
        assert!(same_repository(
            &format!("{owner}/{repo}"),
            "group/sub",
            "repo"
        ));
        assert!(same_repository("Acme/Widgets", "acme", "widgets"));
        assert!(!same_repository("acme/widgets", "acme", "gadgets"));
    }

    #[tokio::test]
    async fn every_push_edits_the_one_summary_comment() {
        let host = FakeHost::default();
        let first = comment(&host).post_or_edit(None, "first").await.unwrap();
        assert_eq!(first, Some(100));

        let second = comment(&host).post_or_edit(first, "second").await.unwrap();
        assert_eq!(second, None);
        assert_eq!(
            *host.comments.lock().unwrap(),
            vec![(100, "second".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_deleted_summary_comment_is_posted_again() {
        let host = FakeHost::default();
        let posted = comment(&host).post_or_edit(Some(42), "body").await.unwrap();
        assert_eq!(posted, Some(100));
        assert_eq!(
            *host.comments.lock().unwrap(),
            vec![(100, "body".to_owned())]
        );
    }
}
