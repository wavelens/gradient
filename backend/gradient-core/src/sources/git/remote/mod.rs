/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Remote HEAD resolution. [`ls_remote_head`] dispatches by URL/credentials to
//! the SSH, HTTPS (libgit2), or raw `git://` wire-protocol implementation.

mod git_protocol;
mod https;
mod ssh;

use crate::sources::SourceError;
use git_protocol::ls_remote_head_git_protocol;
use https::ls_remote_head_no_creds;
use ssh::ls_remote_head_ssh;

/// libgit2 has no built-in SSH host-key verifier; `CertificatePassthrough`
/// gets treated as a rejection ("invalid or unknown remote ssh hostkey").
/// Accept SSH host keys unconditionally (TOFU-less, like
/// `StrictHostKeyChecking=no`); leave HTTPS verification to libgit2's TLS.
pub fn accept_cert(cert: &git2::cert::Cert<'_>) -> git2::CertificateCheckStatus {
    if cert.as_hostkey().is_some() {
        git2::CertificateCheckStatus::CertificateOk
    } else {
        git2::CertificateCheckStatus::CertificatePassthrough
    }
}

pub(super) fn ls_remote_head(
    url: &str,
    private_key: Option<&str>,
    public_key: Option<&str>,
    branch: Option<&str>,
) -> Result<Vec<u8>, SourceError> {
    match (private_key, public_key) {
        (Some(priv_key), Some(pub_key)) => ls_remote_head_ssh(url, priv_key, pub_key, branch),
        _ if url.starts_with("git://") => ls_remote_head_git_protocol(url, branch),
        _ => ls_remote_head_no_creds(url, branch),
    }
}

/// Resolves the target ref from a libgit2 remote ref list.
///
/// `branch = None` → look for `HEAD`, fall back to first ref.
/// `branch = Some("main")` → look for `refs/heads/main` exactly; returns
/// `SourceError::GitHashExtraction` if not found (no HEAD fallback).
fn find_ref_in_list(
    list: &[git2::RemoteHead<'_>],
    branch: Option<&str>,
) -> Result<Vec<u8>, SourceError> {
    match branch {
        None => list
            .iter()
            .find(|h| h.name() == "HEAD")
            .or_else(|| list.first())
            .map(|h| h.oid().as_bytes().to_vec())
            .ok_or(SourceError::GitHashExtraction),
        Some(b) => {
            let ref_name = format!("refs/heads/{}", b);
            list.iter()
                .find(|h| h.name() == ref_name)
                .map(|h| h.oid().as_bytes().to_vec())
                .ok_or(SourceError::GitHashExtraction)
        }
    }
}
