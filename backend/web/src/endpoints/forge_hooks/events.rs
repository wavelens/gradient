/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Forge-agnostic push event parsing and triggering.

use core::types::*;
use serde::Deserialize;
use std::sync::Arc;
use tracing::warn;

use super::trigger::trigger_for_repo_urls;

// ── GitHub push payload ────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct GitHubPushPayload {
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub after: String,
    pub repository: GitHubRepository,
}

#[derive(Deserialize)]
pub(super) struct GitHubRepository {
    pub clone_url: String,
    pub ssh_url: String,
}

// ── Gitea/Forgejo push payload ─────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct GiteaPushPayload {
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub after: String,
    pub repository: GiteaRepository,
}

#[derive(Deserialize)]
pub(super) struct GiteaRepository {
    pub clone_url: String,
    pub ssh_url: Option<String>,
}

// ── GitLab push payload ────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct GitLabPushPayload {
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub after: String,
    pub project: GitLabProject,
}

#[derive(Deserialize)]
pub(super) struct GitLabProject {
    pub http_url: String,
    pub ssh_url: Option<String>,
}

// ── Normalised push event ──────────────────────────────────────────────────

/// Forge-agnostic push event extracted from any of the supported webhook
/// payload shapes. Call `trigger` to queue evaluations.
pub(super) struct ParsedPushEvent {
    pub commit_hash: Vec<u8>,
    pub repository_urls: Vec<String>,
    pub commit_message: Option<String>,
    pub author_name: Option<String>,
}

/// Validates a push event ref/SHA pair and decodes the commit hash.
///
/// Returns `None` for tag pushes, branch deletions (all-zero SHA), or
/// unparseable hex.
pub(super) fn decode_push_commit(git_ref: &str, after: &str, forge: &str) -> Option<Vec<u8>> {
    if !git_ref.starts_with("refs/heads/") || after == "0000000000000000000000000000000000000000" {
        return None;
    }
    match hex::decode(after) {
        Ok(b) => Some(b),
        Err(e) => {
            warn!(error = %e, sha = %after, forge, "Push webhook: invalid commit SHA");
            None
        }
    }
}

impl ParsedPushEvent {
    pub fn from_github(body: &[u8]) -> Option<Self> {
        let payload: GitHubPushPayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse GitHub push payload");
                return None;
            }
        };
        let commit_hash = decode_push_commit(&payload.git_ref, &payload.after, "github")?;
        Some(Self {
            commit_hash,
            repository_urls: vec![payload.repository.clone_url, payload.repository.ssh_url],
            commit_message: None,
            author_name: None,
        })
    }

    pub fn from_gitea(body: &[u8]) -> Option<Self> {
        let payload: GiteaPushPayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse Gitea/Forgejo push payload");
                return None;
            }
        };
        let commit_hash = decode_push_commit(&payload.git_ref, &payload.after, "gitea")?;
        let mut urls = vec![payload.repository.clone_url];
        if let Some(ssh) = payload.repository.ssh_url {
            urls.push(ssh);
        }
        Some(Self {
            commit_hash,
            repository_urls: urls,
            commit_message: None,
            author_name: None,
        })
    }

    pub fn from_gitlab(body: &[u8]) -> Option<Self> {
        let payload: GitLabPushPayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse GitLab push payload");
                return None;
            }
        };
        let commit_hash = decode_push_commit(&payload.git_ref, &payload.after, "gitlab")?;
        let mut urls = vec![payload.project.http_url];
        if let Some(ssh) = payload.project.ssh_url {
            urls.push(ssh);
        }
        Some(Self {
            commit_hash,
            repository_urls: urls,
            commit_message: None,
            author_name: None,
        })
    }

    /// Queue evaluations for all active projects whose repository URL matches.
    pub async fn trigger(self, state: &Arc<ServerState>) {
        let url_refs: Vec<&str> = self.repository_urls.iter().map(String::as_str).collect();
        trigger_for_repo_urls(
            state,
            &url_refs,
            self.commit_hash,
            self.commit_message,
            self.author_name,
        )
        .await;
    }
}
