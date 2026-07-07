/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::remote::accept_cert;
use super::url::parse_nix_git_url;
use crate::{FlakePrefetcher, PrefetchedFlake, SourceError};
use anyhow::Result;
use async_trait::async_trait;
use git2::RemoteCallbacks;
use gradient_types::input::check_repository_url_is_ssh;
use gradient_types::*;
use tracing::{debug, instrument};

/// Production `FlakePrefetcher` backed by libgit2 + the Nix C API.
#[derive(Debug, Default)]
pub struct Libgit2Prefetcher;

impl Libgit2Prefetcher {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl FlakePrefetcher for Libgit2Prefetcher {
    async fn prefetch(
        &self,
        crypt_secret_file: String,
        serve_url: String,
        repository: String,
        organization: MOrganization,
    ) -> Result<Option<PrefetchedFlake>> {
        prefetch_flake_inner(crypt_secret_file, serve_url, repository, organization)
            .await
            .map(|opt| opt.map(PrefetchedFlake::from_tempdir))
            .map_err(|e| anyhow::anyhow!("{}", e))
    }
}

#[instrument(skip(organization), fields(repository = %repository))]
async fn prefetch_flake_inner(
    crypt_secret_file: String,
    serve_url: String,
    repository: String,
    organization: MOrganization,
) -> std::result::Result<Option<tempfile::TempDir>, SourceError> {
    if !check_repository_url_is_ssh(&repository) {
        debug!("HTTPS repository – skipping git clone, nix will fetch on demand");
        return Ok(None);
    }

    debug!(repository, "SSH repository – cloning via libgit2");

    let (private_key, public_key) =
        crate::ssh_key::decrypt_ssh_private_key(&crypt_secret_file, organization, &serve_url)?;

    let (git_url, rev) = parse_nix_git_url(&repository)?;

    let temp_dir = tempfile::TempDir::new().map_err(|e| SourceError::FileRead {
        reason: e.to_string(),
    })?;

    let temp_path = temp_dir.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let fo = make_ssh_fetch_options(&private_key, &public_key);

        let repo = git2::build::RepoBuilder::new()
            .fetch_options(fo)
            .clone(&git_url, &temp_path)
            .map_err(|e| SourceError::GitCommandFailed {
                stderr: e.message().to_string(),
            })?;

        let oid = git2::Oid::from_str(&rev).map_err(|_| SourceError::GitOutputParsing)?;
        let commit = repo
            .find_commit(oid)
            .map_err(|e| SourceError::GitCommandFailed {
                stderr: e.message().to_string(),
            })?;

        let tree = commit.tree().map_err(|e| SourceError::GitCommandFailed {
            stderr: e.message().to_string(),
        })?;

        let mut co = git2::build::CheckoutBuilder::new();
        co.force();
        repo.checkout_tree(tree.as_object(), Some(&mut co))
            .map_err(|e| SourceError::GitCommandFailed {
                stderr: e.message().to_string(),
            })?;

        repo.set_head_detached(oid)
            .map_err(|e| SourceError::GitCommandFailed {
                stderr: e.message().to_string(),
            })?;

        debug!(?temp_path, rev, "Cloned repository");

        gradient_nix::lock_flake_with_ssh_key(&temp_path, &private_key).map_err(|e| {
            SourceError::NixFlakeArchiveFailed {
                stderr: e.to_string(),
            }
        })?;

        debug!(?temp_path, "Locked flake and prefetched inputs");

        Ok::<(), SourceError>(())
    })
    .await
    .map_err(|e| SourceError::GitExecution {
        error: e.to_string(),
    })??;

    Ok(Some(temp_dir))
}

/// Build `FetchOptions` with in-memory SSH credentials.
/// The key strings are cloned so the closure is `'static`.
fn make_ssh_fetch_options(private_key: &str, public_key: &str) -> git2::FetchOptions<'static> {
    let priv_key = private_key.to_owned();
    let pub_key = public_key.to_owned();
    let mut callbacks = RemoteCallbacks::new();
    callbacks.certificate_check(|cert, _valid| Ok(accept_cert(cert)));
    callbacks.credentials(move |_url, username_from_url, _allowed| {
        git2::Cred::ssh_key_from_memory(
            username_from_url.unwrap_or("git"),
            Some(&pub_key),
            &priv_key,
            None,
        )
    });

    let mut fo = git2::FetchOptions::new();
    fo.remote_callbacks(callbacks);
    fo
}
