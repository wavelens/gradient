/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::fixtures::run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout;
use crate::git::changed_paths::{
    ComparisonBaseCommit, fetch_base_and_head_into_temp_repo_and_get_changed_paths,
};
use crate::git::git_credentials::GitOperationCredentials;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn create_temporary_git_repository_on_main_with_initial_commit_of_files(
    files: &[&str],
) -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("r");
    std::fs::create_dir(&repo).unwrap();
    run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
        &repo,
        &["init", "-b", "main"],
    );
    for file in files {
        std::fs::write(repo.join(file), file).unwrap();
    }
    stage_all_changes_commit_and_get_commit_hash_bytes(&repo);
    (tmp, repo)
}

fn stage_all_changes_commit_and_get_commit_hash_bytes(repo: &Path) -> Vec<u8> {
    run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(repo, &["add", "-A"]);
    run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
        repo,
        &["commit", "-m", "change"],
    );
    hex::decode(
        run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
            repo,
            &["rev-parse", "HEAD"],
        ),
    )
    .unwrap()
}

fn url(repo: &Path) -> String {
    format!("file://{}", repo.display())
}

fn changed(
    base_repo: &Path,
    base: &ComparisonBaseCommit,
    head_repo: &Path,
    head: &[u8],
) -> Result<Vec<String>, crate::SourceError> {
    fetch_base_and_head_into_temp_repo_and_get_changed_paths(
        &url(base_repo),
        base,
        &url(head_repo),
        head,
        &GitOperationCredentials::default(),
    )
}

#[test]
fn changed_paths_returns_added_modified_deleted_and_renamed_old_and_new_paths_between_commits() {
    let (_tmp, repo) = create_temporary_git_repository_on_main_with_initial_commit_of_files(&[
        "kept", "modified", "deleted", "renamed",
    ]);
    let base = hex::decode(
        run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
            &repo,
            &["rev-parse", "HEAD"],
        ),
    )
    .unwrap();
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/added"), "added").unwrap();
    std::fs::write(repo.join("modified"), "changed contents").unwrap();
    run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
        &repo,
        &["rm", "-q", "deleted"],
    );
    run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
        &repo,
        &["mv", "renamed", "moved"],
    );
    let head = stage_all_changes_commit_and_get_commit_hash_bytes(&repo);

    let paths = changed(
        &repo,
        &ComparisonBaseCommit::CommitHashBytes(base),
        &repo,
        &head,
    )
    .unwrap();

    assert_eq!(
        paths,
        ["deleted", "modified", "moved", "renamed", "src/added"]
    );
}

#[test]
fn changed_paths_returns_error_when_base_commit_does_not_exist() {
    let (_tmp, repo) = create_temporary_git_repository_on_main_with_initial_commit_of_files(&["f"]);
    std::fs::write(repo.join("f"), "changed").unwrap();
    let head = stage_all_changes_commit_and_get_commit_hash_bytes(&repo);

    let result = changed(
        &repo,
        &ComparisonBaseCommit::CommitHashBytes(vec![0xab; 20]),
        &repo,
        &head,
    );

    assert!(result.is_err(), "{result:?}");
}

#[test]
fn changed_paths_with_main_branch_base_and_fork_head_returns_changed_file() {
    let (_upstream_tmp, upstream) =
        create_temporary_git_repository_on_main_with_initial_commit_of_files(&["docs", "src"]);
    let fork_tmp = tempfile::tempdir().unwrap();
    let fork = fork_tmp.path().join("fork");
    run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
        fork_tmp.path(),
        &["clone", "-q", &url(&upstream), fork.to_str().unwrap()],
    );
    std::fs::write(fork.join("src"), "changed in the fork").unwrap();
    let head = stage_all_changes_commit_and_get_commit_hash_bytes(&fork);

    let paths = changed(
        &upstream,
        &ComparisonBaseCommit::BranchName("main".into()),
        &fork,
        &head,
    )
    .unwrap();

    assert_eq!(paths, ["src"]);
}
