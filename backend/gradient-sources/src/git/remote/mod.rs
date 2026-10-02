/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod git_protocol;
mod https;
mod ssh;

use super::url::git_transport_url;
use crate::SourceError;
use git_protocol::ls_remote_head_git_protocol;
use git2::RemoteCallbacks;
use https::ls_remote_head_no_creds;
use ssh::ls_remote_head_ssh;

/// libgit2 has no SSH host-key verifier and is treating `CertificatePassthrough` as a rejection.
/// SSH host keys are accepted unconditionally, and HTTPS verification is left to libgit2's TLS.
pub fn accept_cert(cert: &git2::cert::Cert<'_>) -> git2::CertificateCheckStatus {
    if cert.as_hostkey().is_some() {
        git2::CertificateCheckStatus::CertificateOk
    } else {
        git2::CertificateCheckStatus::CertificatePassthrough
    }
}

pub fn fetch_options_with_ssh(ssh_key: Option<&str>) -> git2::FetchOptions<'static> {
    let mut callbacks = RemoteCallbacks::new();
    callbacks.certificate_check(|cert, _valid| Ok(accept_cert(cert)));

    if let Some(key) = ssh_key {
        let key = key.to_owned();
        callbacks.credentials(move |_url, username_from_url, _allowed| {
            git2::Cred::ssh_key_from_memory(username_from_url.unwrap_or("git"), None, &key, None)
        });
    }

    let mut fo = git2::FetchOptions::new();
    fo.remote_callbacks(callbacks);
    fo
}

pub(in crate::git) fn ls_remote_head(
    url: &str,
    private_key: Option<&str>,
    public_key: Option<&str>,
    branch: Option<&str>,
) -> Result<Vec<u8>, SourceError> {
    let url = git_transport_url(url);
    match (private_key, public_key) {
        (Some(priv_key), Some(pub_key)) => ls_remote_head_ssh(url, priv_key, pub_key, branch),
        _ if url.starts_with("git://") => ls_remote_head_git_protocol(url, branch),
        _ => ls_remote_head_no_creds(url, branch),
    }
}

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
