/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::*;
use crate::input::client_from_config;
use crate::output::{ExitKind, Output, to_exit_kind};
use clap::Subcommand;
use connector::api::CreatePullRequestRequest;
use git2::{DiffFormat, DiffOptions, Oid, Repository};
use std::path::PathBuf;

const AGENT: &str = "http://127.0.0.1:7777";

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Open or update a pull request with the working tree changes of the selected task
    Open {
        /// Pull request title
        #[arg(long)]
        title: String,
        /// File holding the pull request body
        #[arg(long)]
        body: Option<PathBuf>,
        /// Branch to push (default: derived from the touched paths)
        #[arg(long)]
        branch: Option<String>,
        /// Branch the pull request targets (default: the repository's default branch)
        #[arg(long)]
        base: Option<String>,
        /// Paths to include, untracked files too (default: every change)
        paths: Vec<String>,
    },
}

pub async fn handle(cmd: Commands, out: Output) {
    match cmd {
        Commands::Open {
            title,
            body,
            branch,
            base,
            paths,
        } => {
            let request = OpenRequest {
                title,
                body,
                branch,
                base,
                paths,
            };
            open(request, out).await
        }
    }
}

struct OpenRequest {
    title: String,
    body: Option<PathBuf>,
    branch: Option<String>,
    base: Option<String>,
    paths: Vec<String>,
}

async fn open(request: OpenRequest, out: Output) {
    let (project, task) = selected_task(out);
    let in_job = std::env::var_os("GRADIENT_JOB").is_some();
    let repo = Repository::discover(".")
        .unwrap_or_else(|e| out.err(ExitKind::Usage, format!("no git repository here: {e}")));
    let env_commit = std::env::var("GRADIENT_COMMIT").ok();
    let start = start_commit(&repo, env_commit.as_deref(), request.base.as_deref())
        .unwrap_or_else(|e| out.err(ExitKind::Usage, e));
    let patch = working_tree_patch(&repo, start, &request.paths)
        .unwrap_or_else(|e| out.err(ExitKind::Api, format!("diff the working tree: {e}")));
    if patch.is_empty() {
        out.err(ExitKind::Api, "Nothing to open a pull request for.");
    }
    let patch = if in_job {
        connector::pull_requests::redact_through_agent(AGENT, patch)
            .await
            .unwrap_or_else(|e| out.err(to_exit_kind(&e), e))
    } else {
        patch
    };

    let body = CreatePullRequestRequest {
        title: request.title,
        body: request.body.map(|file| read_body(&file, out)),
        branch: request.branch,
        base: request.base,
        patch: String::from_utf8_lossy(&patch).into_owned(),
        commit: (!in_job).then(|| start.to_string()),
    };
    match client_from_config(out)
        .pull_requests()
        .open(&project, &task, body)
        .await
    {
        Ok(opened) => {
            out.ok(&opened);
            out.human(&opened.url);
        }
        Err(e) => out.err(to_exit_kind(&e), e),
    }
}

fn selected_task(out: Output) -> (String, String) {
    get_value(ConfigKey::SelectedTask, true)
        .as_deref()
        .and_then(|selected| selected.split_once('/'))
        .map(|(project, task)| (project.to_owned(), task.to_owned()))
        .unwrap_or_else(|| {
            out.err(
                ExitKind::Usage,
                "Select a task with 'gradient task select <name>' or run inside an exec job.",
            )
        })
}

fn read_body(file: &PathBuf, out: Output) -> String {
    std::fs::read_to_string(file).unwrap_or_else(|e| {
        out.err(
            ExitKind::Usage,
            format!("read the body file {}: {e}", file.display()),
        )
    })
}

pub(crate) fn start_commit(
    repo: &Repository,
    env_commit: Option<&str>,
    base: Option<&str>,
) -> Result<Oid, String> {
    if let Some(commit) = env_commit {
        return Oid::from_str(commit).map_err(|e| format!("GRADIENT_COMMIT {commit}: {e}"));
    }
    merge_base_with_origin(repo, base).map_err(|e| {
        format!(
            "no start commit ({}): set GRADIENT_COMMIT or pass --base naming a branch of the origin remote",
            e.message()
        )
    })
}

fn merge_base_with_origin(repo: &Repository, base: Option<&str>) -> Result<Oid, git2::Error> {
    let head = repo.head()?.peel_to_commit()?.id();
    let remote_ref = format!("refs/remotes/origin/{}", base.unwrap_or("HEAD"));
    let remote = repo
        .find_reference(&remote_ref)?
        .resolve()?
        .peel_to_commit()?
        .id();
    repo.merge_base(head, remote)
}

pub(crate) fn working_tree_patch(
    repo: &Repository,
    start: Oid,
    paths: &[String],
) -> Result<Vec<u8>, git2::Error> {
    let start_tree = repo.find_commit(start)?.tree()?;
    let mut options = DiffOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .show_binary(true);
    for path in paths {
        options.pathspec(path);
    }
    let diff = repo.diff_tree_to_workdir_with_index(Some(&start_tree), Some(&mut options))?;
    let mut patch = Vec::new();
    diff.print(DiffFormat::Patch, |_, _, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            patch.push(line.origin() as u8);
        }
        patch.extend_from_slice(line.content());
        true
    })?;
    Ok(patch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Signature;
    use tempfile::TempDir;

    fn repo_with(files: &[(&str, &str)]) -> (TempDir, Repository, Oid) {
        let dir = TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        for (path, contents) in files {
            std::fs::write(dir.path().join(path), contents).unwrap();
        }
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = Signature::now("t", "t@example.com").unwrap();
        let start = repo
            .commit(Some("HEAD"), &signature, &signature, "init", &tree, &[])
            .unwrap();
        drop(tree);
        (dir, repo, start)
    }

    #[test]
    fn pr_open_includes_untracked_listed_paths() {
        let (dir, repo, start) = repo_with(&[("flake.lock", "a"), ("README", "r")]);
        std::fs::write(dir.path().join("flake.lock"), "b").unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), "new").unwrap();
        std::fs::write(dir.path().join("README"), "changed").unwrap();

        let patch = String::from_utf8(
            working_tree_patch(&repo, start, &["flake.lock".into(), "Cargo.lock".into()]).unwrap(),
        )
        .unwrap();

        assert!(patch.contains("+++ b/flake.lock") && patch.contains("+++ b/Cargo.lock"));
        assert!(!patch.contains("README"));
    }

    #[test]
    fn the_start_commit_comes_from_the_job_context_first() {
        let (_dir, repo, start) = repo_with(&[("a", "1")]);

        assert_eq!(
            start_commit(&repo, Some(&start.to_string()), None).unwrap(),
            start
        );
        assert!(
            start_commit(&repo, None, None)
                .unwrap_err()
                .contains("GRADIENT_COMMIT")
        );
    }
}
