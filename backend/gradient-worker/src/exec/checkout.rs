/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use git2::ErrorClass;
use gradient_types::RepositoryLocation;
use gradient_types::secret::SecretString;
use gradient_wire::types::JobContext;
use tokio_util::sync::CancellationToken;

use super::job::{STORE_DIR, StoreAccess};
use crate::executor::failure::BuildError;
use crate::proto::credentials::JobCredentials;
use crate::proto::job::JobUpdater;
use crate::proto::prefetch::{MissingInputs, SubstituteNotOnUpstream, ensure_path};

#[derive(Debug, PartialEq, Eq)]
pub enum WorkSource<'a> {
    StorePath(&'a str),
    Git { url: &'a str, commit: &'a str },
}

impl<'a> WorkSource<'a> {
    pub fn of(context: &'a JobContext) -> Self {
        let clone_url = RepositoryLocation::of(&context.repository).clone_url;
        match clone_url.starts_with(STORE_DIR) {
            true => Self::StorePath(clone_url),
            false => Self::Git {
                url: &context.repository,
                commit: &context.commit,
            },
        }
    }
}

pub async fn checkout_work(
    work: &Path,
    source: WorkSource<'_>,
    nix: &StoreAccess<'_>,
    credentials: &JobCredentials,
    updater: &mut JobUpdater,
) -> Result<(), BuildError> {
    match source {
        WorkSource::StorePath(store_path) => {
            let _root = nix.gcroots.add(store_path).await;
            ensure_path(nix.store, store_path, updater)
                .await
                .map_err(store_path_failure)?;
            copy_store_path(work, store_path)
                .await
                .map_err(BuildError::transient)
        }
        WorkSource::Git { url, commit } => clone_commit(work, url, commit, credentials)
            .await
            .map_err(git_failure),
    }
}

async fn clone_commit(
    work: &Path,
    url: &str,
    commit: &str,
    credentials: &JobCredentials,
) -> Result<()> {
    let ssh_key = project_ssh_key(credentials)?;
    let (dir, url, commit) = (work.to_path_buf(), url.to_owned(), commit.to_owned());
    tokio::task::spawn_blocking(move || {
        gradient_sources::checkout_commit_into(
            &dir,
            &url,
            &commit,
            ssh_key.as_ref().map(SecretString::expose),
        )
    })
    .await
    .context("checkout task panicked")?
}

async fn copy_store_path(work: &Path, store_path: &str) -> Result<()> {
    let (from, to) = (PathBuf::from(store_path), work.to_path_buf());
    let stopped = CancellationToken::new();
    let _stop_when_dropped = stopped.clone().drop_guard();
    tokio::task::spawn_blocking(move || copy_writable_tree(&from, &to, &stopped))
        .await
        .context("copy task panicked")?
        .with_context(|| format!("copy {store_path} into the work directory"))
}

fn copy_writable_tree(from: &Path, to: &Path, stopped: &CancellationToken) -> std::io::Result<()> {
    for entry in std::fs::read_dir(from)? {
        if stopped.is_cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "Job stopped during copy to work directory",
            ));
        }
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            std::fs::create_dir(&target)?;
            copy_writable_tree(&source, &target, stopped)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&source)?, &target)?;
        } else {
            std::fs::copy(&source, &target)?;
            let mut permissions = std::fs::metadata(&target)?.permissions();
            permissions.set_mode(permissions.mode() | 0o200);
            std::fs::set_permissions(&target, permissions)?;
        }
    }
    Ok(())
}

fn store_path_failure(error: anyhow::Error) -> BuildError {
    let missing_everywhere = error
        .chain()
        .any(|cause| cause.is::<MissingInputs>() || cause.is::<SubstituteNotOnUpstream>());
    match missing_everywhere {
        true => BuildError::permanent(error),
        false => BuildError::transient(error),
    }
}

fn git_failure(error: anyhow::Error) -> BuildError {
    let fails_on_every_worker = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<git2::Error>())
        .is_some_and(|git| {
            matches!(
                git.class(),
                ErrorClass::Invalid
                    | ErrorClass::Repository
                    | ErrorClass::Reference
                    | ErrorClass::Object
                    | ErrorClass::Odb
            )
        });
    match fails_on_every_worker {
        true => BuildError::permanent(error),
        false => BuildError::transient(error),
    }
}

fn project_ssh_key(credentials: &JobCredentials) -> Result<Option<SecretString>> {
    credentials
        .ssh_key()
        .map(|key| SecretString::from_utf8(key).context("project SSH key is not UTF-8"))
        .transpose()
}

#[cfg(test)]
mod tests {
    use gradient_wire::messages::BuildFailureKind;

    use super::*;

    const NO_COMMIT: &str = "0000000000000000000000000000000000000000";

    fn context(repository: &str, commit: &str) -> JobContext {
        JobContext {
            repository: repository.into(),
            commit: commit.into(),
            ..Default::default()
        }
    }

    #[test]
    fn an_uploaded_source_is_copied_from_the_store_and_a_repository_is_cloned() {
        let uploaded = context("/nix/store/aaaa-source", NO_COMMIT);
        assert_eq!(
            WorkSource::of(&uploaded),
            WorkSource::StorePath("/nix/store/aaaa-source")
        );
        let repository = context("https://h/o/r?dir=nix", "abc123");
        assert_eq!(
            WorkSource::of(&repository),
            WorkSource::Git {
                url: "https://h/o/r?dir=nix",
                commit: "abc123"
            }
        );
    }

    #[test]
    fn cloning_a_directory_that_is_no_repository_fails_the_job() {
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("flake.nix"), "{}").unwrap();
        let work = tempfile::tempdir().unwrap();
        let error = gradient_sources::checkout_commit_into(
            work.path(),
            &source.path().to_string_lossy(),
            NO_COMMIT,
            None,
        )
        .unwrap_err();
        assert_eq!(git_failure(error).kind, BuildFailureKind::Permanent);
    }

    #[test]
    fn an_unreachable_remote_moves_the_job_to_another_worker() {
        let unreachable = git2::Error::new(
            git2::ErrorCode::GenericError,
            ErrorClass::Net,
            "failed to resolve address for gitea.example",
        );
        let error = anyhow::Error::new(unreachable).context("fetch commit abc from gitea.example");
        assert_eq!(git_failure(error).kind, BuildFailureKind::Transient);
    }

    #[test]
    fn a_source_missing_from_every_cache_fails_the_job() {
        let missing = anyhow::Error::new(MissingInputs(vec!["/nix/store/aaaa-source".into()]));
        assert_eq!(
            store_path_failure(missing).kind,
            BuildFailureKind::Permanent
        );
        let dropped = anyhow::anyhow!("connection reset by peer");
        assert_eq!(
            store_path_failure(dropped).kind,
            BuildFailureKind::Transient
        );
    }

    #[test]
    fn the_copied_store_tree_is_writable_and_keeps_its_links() {
        let store = tempfile::tempdir().unwrap();
        std::fs::create_dir(store.path().join("src")).unwrap();
        let file = store.path().join("src/main.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::os::unix::fs::symlink("src/main.rs", store.path().join("link")).unwrap();
        let work = tempfile::tempdir().unwrap();

        copy_writable_tree(store.path(), work.path(), &CancellationToken::new()).unwrap();

        std::fs::write(work.path().join("src/main.rs"), "changed").unwrap();
        assert_eq!(
            std::fs::read_link(work.path().join("link")).unwrap(),
            PathBuf::from("src/main.rs")
        );
    }

    #[test]
    fn a_stopped_job_copies_nothing_more() {
        let store = tempfile::tempdir().unwrap();
        std::fs::write(store.path().join("big.bin"), "x").unwrap();
        let work = tempfile::tempdir().unwrap();
        let stopped = CancellationToken::new();
        stopped.cancel();

        let error = copy_writable_tree(store.path(), work.path(), &stopped).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(!work.path().join("big.bin").exists());
    }
}
