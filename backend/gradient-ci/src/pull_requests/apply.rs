/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::PullRequestError;
use git2::{ApplyLocation, Delta, Diff, DiffDelta, Repository};
use gradient_git_host::CommitFile;
use std::path::Path;

pub fn patched_files(checkout: &Path, patch: &[u8]) -> Result<Vec<CommitFile>, PullRequestError> {
    if patch.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    let diff = Diff::from_buffer(patch).map_err(does_not_apply)?;
    let touched = touched_paths(&diff)?;
    let repo = Repository::open(checkout).map_err(does_not_apply)?;
    repo.apply(&diff, ApplyLocation::WorkDir, None)
        .map_err(does_not_apply)?;
    touched
        .into_iter()
        .map(|path| read_new_contents(checkout, path))
        .collect()
}

pub fn patch_paths(patch: &[u8]) -> Result<Vec<String>, PullRequestError> {
    if patch.iter().all(u8::is_ascii_whitespace) {
        return Err(PullRequestError::Empty);
    }
    let diff = Diff::from_buffer(patch).map_err(does_not_apply)?;
    touched_paths(&diff)
}

fn touched_paths(diff: &Diff<'_>) -> Result<Vec<String>, PullRequestError> {
    diff.deltas().map(|delta| written_path(&delta)).collect()
}

fn written_path(delta: &DiffDelta<'_>) -> Result<String, PullRequestError> {
    let old = path_text(delta.old_file().path());
    match delta.status() {
        Delta::Deleted | Delta::Renamed => Err(PullRequestError::DeletesFile(old)),
        _ => Ok(path_text(delta.new_file().path())),
    }
}

fn path_text(path: Option<&Path>) -> String {
    path.map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn read_new_contents(checkout: &Path, path: String) -> Result<CommitFile, PullRequestError> {
    let contents = std::fs::read(checkout.join(&path))
        .map_err(|e| PullRequestError::DoesNotApply(format!("read {path}: {e}")))?;
    Ok(CommitFile { path, contents })
}

fn does_not_apply(error: git2::Error) -> PullRequestError {
    PullRequestError::DoesNotApply(error.message().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{DiffOptions, Signature};
    use tempfile::TempDir;

    fn repo_with(files: &[(&str, &str)]) -> (TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
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
        repo.commit(Some("HEAD"), &signature, &signature, "init", &tree, &[])
            .unwrap();
        drop(tree);
        (dir, repo)
    }

    fn working_tree_patch(repo: &Repository) -> Vec<u8> {
        let head = repo.head().unwrap().peel_to_tree().unwrap();
        let mut options = DiffOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true);
        let diff = repo
            .diff_tree_to_workdir(Some(&head), Some(&mut options))
            .unwrap();
        let mut patch = Vec::new();
        diff.print(git2::DiffFormat::Patch, |_, _, line| {
            if matches!(line.origin(), '+' | '-' | ' ') {
                patch.push(line.origin() as u8);
            }
            patch.extend_from_slice(line.content());
            true
        })
        .unwrap();
        patch
    }

    #[test]
    fn a_patch_yields_the_new_contents_of_each_touched_file() {
        let files = [("flake.lock", "a\n"), ("README", "r\n")];
        let (checkout, _) = repo_with(&files);
        let (edited, repo) = repo_with(&files);
        std::fs::write(edited.path().join("flake.lock"), "b\n").unwrap();
        std::fs::write(edited.path().join("new.txt"), "new\n").unwrap();

        let mut written = patched_files(checkout.path(), &working_tree_patch(&repo)).unwrap();
        written.sort_by(|a, b| a.path.cmp(&b.path));

        assert_eq!(
            written,
            vec![
                CommitFile {
                    path: "flake.lock".into(),
                    contents: b"b\n".to_vec()
                },
                CommitFile {
                    path: "new.txt".into(),
                    contents: b"new\n".to_vec()
                },
            ]
        );
    }

    #[test]
    fn a_patch_that_deletes_a_file_is_refused_with_its_path() {
        let files = [("flake.lock", "a\n"), ("README", "r\n")];
        let (checkout, _) = repo_with(&files);
        let (edited, repo) = repo_with(&files);
        std::fs::remove_file(edited.path().join("README")).unwrap();

        let refused = patched_files(checkout.path(), &working_tree_patch(&repo));

        assert!(
            matches!(&refused, Err(PullRequestError::DeletesFile(path)) if path == "README"),
            "{refused:?}"
        );
    }

    #[test]
    fn a_patch_for_another_tree_does_not_apply() {
        let (checkout, _) = repo_with(&[("flake.lock", "a\n")]);
        let (edited, repo) = repo_with(&[("flake.lock", "zzz\n")]);
        std::fs::write(edited.path().join("flake.lock"), "b\n").unwrap();

        let refused = patched_files(checkout.path(), &working_tree_patch(&repo));

        assert!(
            matches!(refused, Err(PullRequestError::DoesNotApply(_))),
            "{refused:?}"
        );
    }
}
