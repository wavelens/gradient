/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::{Path, PathBuf};
use std::sync::Once;

use bytes::Bytes;
use git2::{DiffFormat, DiffOptions, Repository};
use gradient_redact::SecretRedactor;
use gradient_wire::types::TREE_PATCH_OUTPUT;

use crate::http::HttpState;
use crate::run::note;

static FOREIGN_OWNER_ALLOWED: Once = Once::new();

pub fn tree_patch(
    work: &Path,
    start_commit: &str,
    paths: &[String],
) -> Result<Vec<u8>, git2::Error> {
    allow_foreign_owner();
    let repo = Repository::open(work)?;
    let start = repo.revparse_single(start_commit)?.peel_to_tree()?;
    let mut options = DiffOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .show_binary(true);
    for path in paths {
        options.pathspec(path);
    }
    let diff = repo.diff_tree_to_workdir_with_index(Some(&start), Some(&mut options))?;
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

fn allow_foreign_owner() {
    FOREIGN_OWNER_ALLOWED.call_once(|| {
        // SAFETY: called once, before any repository is opened; /work belongs to the program user.
        if let Err(e) = unsafe { git2::opts::set_verify_owner_validation(false) } {
            tracing::warn!(error = %e, "cannot turn off the repository owner check");
        }
    });
}

pub async fn upload_tree_patch(
    work: &Path,
    start_commit: &str,
    redactor: &SecretRedactor,
    http: &HttpState,
) {
    if !work.join(".git").exists() {
        note(
            &http.output,
            &format!("no git checkout in {}, no tree patch", work.display()),
        )
        .await;
        return;
    }
    let patch = match patch_of_checkout(work.to_owned(), start_commit.to_owned()).await {
        Ok(patch) if patch.is_empty() => return,
        Ok(patch) => Bytes::from(redactor.redact(&patch)),
        Err(reason) => {
            note(
                &http.output,
                &format!("cannot diff the tree for {TREE_PATCH_OUTPUT}: {reason}"),
            )
            .await;
            return;
        }
    };
    if let Err(failure) = http
        .uploads
        .upload_bytes(&http.link, TREE_PATCH_OUTPUT, patch)
        .await
    {
        note(
            &http.output,
            &format!("cannot upload {TREE_PATCH_OUTPUT}: {failure}"),
        )
        .await;
    }
}

async fn patch_of_checkout(work: PathBuf, start_commit: String) -> Result<Vec<u8>, String> {
    tokio::task::spawn_blocking(move || tree_patch(&work, &start_commit, &[]))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.message().to_owned())
}

#[cfg(test)]
mod tests {
    use git2::{ApplyLocation, Diff, Signature};

    use super::*;

    fn repo_with(files: &[(&str, &str)]) -> (tempfile::TempDir, Repository, String) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        for (name, content) in files {
            std::fs::write(dir.path().join(name), content).unwrap();
        }
        let commit = {
            let mut index = repo.index().unwrap();
            for (name, _) in files {
                index.add_path(Path::new(name)).unwrap();
            }
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let signature = Signature::now("gradient", "gradient@example.com").unwrap();
            repo.commit(Some("HEAD"), &signature, &signature, "start", &tree, &[])
                .unwrap()
        };
        (dir, repo, commit.to_string())
    }

    fn start_files() -> [(&'static str, &'static str); 2] {
        [("flake.lock", "lock-a\n"), ("README", "readme\n")]
    }

    #[test]
    fn an_unchanged_tree_has_no_patch() {
        let (dir, _repo, start) = repo_with(&start_files());

        assert!(tree_patch(dir.path(), &start, &[]).unwrap().is_empty());
    }

    #[test]
    fn a_new_untracked_file_is_in_the_tree_patch() {
        let (dir, _repo, start) = repo_with(&start_files());
        std::fs::write(dir.path().join("new.lock"), "new\n").unwrap();

        let patch = String::from_utf8(tree_patch(dir.path(), &start, &[]).unwrap()).unwrap();

        assert!(patch.contains("+++ b/new.lock"), "{patch}");
    }

    #[test]
    fn a_modified_file_applies_back_on_the_start_commit() {
        let (dir, _repo, start) = repo_with(&start_files());
        std::fs::write(dir.path().join("flake.lock"), "lock-b\n").unwrap();
        let patch = tree_patch(dir.path(), &start, &[]).unwrap();
        let (fresh, fresh_repo, _) = repo_with(&start_files());

        fresh_repo
            .apply(
                &Diff::from_buffer(&patch).unwrap(),
                ApplyLocation::WorkDir,
                None,
            )
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(fresh.path().join("flake.lock")).unwrap(),
            "lock-b\n"
        );
    }

    #[test]
    fn listed_paths_limit_the_patch() {
        let (dir, _repo, start) = repo_with(&start_files());
        std::fs::write(dir.path().join("flake.lock"), "lock-b\n").unwrap();
        std::fs::write(dir.path().join("README"), "changed\n").unwrap();

        let patch = tree_patch(dir.path(), &start, &["flake.lock".into()]).unwrap();
        let patch = String::from_utf8(patch).unwrap();

        assert!(patch.contains("flake.lock"), "{patch}");
        assert!(!patch.contains("README"), "{patch}");
    }
}
