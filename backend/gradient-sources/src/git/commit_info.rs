/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::context::ProjectGitContext;
use super::remote::accept_cert;
use crate::SourceError;
use gradient_types::input::vec_to_hex;
use git2::RemoteCallbacks;
use tracing::{debug, instrument};

impl ProjectGitContext<'_> {
    /// Clone the repository at `commit_hash` and extract the commit metadata.
    ///
    /// Returns `(message, author_email, author_name)`.
    #[instrument(skip(self), fields(project_id = %self.project.id, project_name = %self.project.name, commit_hash = %vec_to_hex(commit_hash)))]
    pub(super) async fn commit_info(
        &self,
        commit_hash: &[u8],
    ) -> Result<(String, Option<String>, String), SourceError> {
        debug!("Fetching commit info");

        let hash_str = vec_to_hex(commit_hash);
        let url = self.project.repository.clone();
        let ssh_creds = self.ssh_creds.clone();

        let temp_dir = tempfile::TempDir::new().map_err(|e| SourceError::FileRead {
            reason: e.to_string(),
        })?;

        let temp_path = temp_dir.path().to_path_buf();

        tokio::task::spawn_blocking(move || {
            let mut callbacks = RemoteCallbacks::new();
            callbacks.certificate_check(|cert, _valid| Ok(accept_cert(cert)));

            if let Some((private_key, public_key)) = ssh_creds {
                callbacks.credentials(move |_url, username_from_url, _allowed| {
                    git2::Cred::ssh_key_from_memory(
                        username_from_url.unwrap_or("git"),
                        Some(&public_key),
                        &private_key,
                        None,
                    )
                });
            }

            let mut fo = git2::FetchOptions::new();
            fo.remote_callbacks(callbacks);

            let mut builder = git2::build::RepoBuilder::new();
            builder.bare(true);
            builder.fetch_options(fo);
            let repo =
                builder
                    .clone(&url, &temp_path)
                    .map_err(|e| SourceError::GitCommandFailed {
                        stderr: e.message().to_string(),
                    })?;

            let oid = git2::Oid::from_str(&hash_str).map_err(|_| SourceError::GitOutputParsing)?;
            let commit = repo
                .find_commit(oid)
                .map_err(|e| SourceError::GitCommandFailed {
                    stderr: e.message().to_string(),
                })?;

            let message = commit.summary().unwrap_or("").to_string();
            let author_email = commit.author().email().map(|s| s.to_string());
            let author_name = commit.author().name().unwrap_or("").to_string();

            Ok((message, author_email, author_name))
        })
        .await
        .map_err(|e| SourceError::GitExecution {
            error: e.to_string(),
        })?
    }
}
