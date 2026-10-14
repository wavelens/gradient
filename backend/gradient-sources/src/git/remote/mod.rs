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
use gradient_types::branch_key;
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
    let mut fo = git2::FetchOptions::new();
    fo.remote_callbacks(callbacks_with_ssh(ssh_key));
    fo
}

pub(in crate::git) fn callbacks_with_ssh(ssh_key: Option<&str>) -> RemoteCallbacks<'static> {
    let mut callbacks = RemoteCallbacks::new();
    callbacks.certificate_check(|cert, _valid| Ok(accept_cert(cert)));

    if let Some(key) = ssh_key {
        let key = key.to_owned();
        callbacks.credentials(move |_url, username_from_url, _allowed| {
            git2::Cred::ssh_key_from_memory(username_from_url.unwrap_or("git"), None, &key, None)
        });
    }

    callbacks
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchHead {
    pub branch: String,
    pub commit: Vec<u8>,
}

pub(in crate::git) struct AdvertisedRef {
    pub name: String,
    pub oid: Vec<u8>,
    pub symref_target: Option<String>,
}

pub(in crate::git) fn ls_remote_head(
    url: &str,
    private_key: Option<&str>,
    public_key: Option<&str>,
    branch: Option<&str>,
) -> Result<BranchHead, SourceError> {
    let url = git_transport_url(url);
    match (private_key, public_key) {
        (Some(priv_key), Some(pub_key)) => ls_remote_head_ssh(url, priv_key, pub_key, branch),
        _ if url.starts_with("git://") => ls_remote_head_git_protocol(url, branch),
        _ => ls_remote_head_no_creds(url, branch),
    }
}

fn advertised(list: &[git2::RemoteHead<'_>]) -> Vec<AdvertisedRef> {
    list.iter()
        .map(|head| AdvertisedRef {
            name: head.name().to_owned(),
            oid: head.oid().as_bytes().to_vec(),
            symref_target: head.symref_target().map(str::to_owned),
        })
        .collect()
}

pub(in crate::git) fn branch_head(
    refs: &[AdvertisedRef],
    branch: Option<&str>,
) -> Result<BranchHead, SourceError> {
    match branch {
        Some(name) => tip_of(refs, branch_key::branch(name)),
        None => default_branch_head(refs),
    }
    .ok_or(SourceError::GitHashExtraction)
}

fn tip_of(refs: &[AdvertisedRef], name: String) -> Option<BranchHead> {
    let tip = refs.iter().find(|r| r.name == name)?;
    Some(BranchHead {
        commit: tip.oid.clone(),
        branch: name,
    })
}

fn default_branch_head(refs: &[AdvertisedRef]) -> Option<BranchHead> {
    let Some(head) = refs.iter().find(|r| r.name == "HEAD") else {
        let first = branches(refs).next()?;
        return tip_of(refs, first.name.clone());
    };
    let branch = head
        .symref_target
        .clone()
        .or_else(|| branch_at_commit(refs, &head.oid))
        .unwrap_or_else(|| branch_key::NO_BRANCH.to_owned());
    Some(BranchHead {
        branch,
        commit: head.oid.clone(),
    })
}

fn branches(refs: &[AdvertisedRef]) -> impl Iterator<Item = &AdvertisedRef> {
    refs.iter().filter(|r| branch_key::is_branch(&r.name))
}

fn branch_at_commit(refs: &[AdvertisedRef], commit: &[u8]) -> Option<String> {
    branches(refs)
        .find(|r| r.oid == commit)
        .map(|r| r.name.clone())
}
