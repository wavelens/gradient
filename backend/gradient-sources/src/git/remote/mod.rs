/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod git_protocol;

use super::ssh::with_private_key;
use super::url::git_transport_url;
use crate::SourceError;
use git_protocol::ls_remote_head_git_protocol;
use git2::Direction;
use gradient_types::branch_key;

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
    branch: Option<&str>,
) -> Result<BranchHead, SourceError> {
    let url = git_transport_url(url);
    if url.starts_with("git://") && private_key.is_none() {
        return ls_remote_head_git_protocol(url, branch);
    }

    with_private_key(private_key, || ls_remote_head_libgit2(url, branch))
}

fn ls_remote_head_libgit2(url: &str, branch: Option<&str>) -> Result<BranchHead, SourceError> {
    let git_error = |e: git2::Error| SourceError::GitCommandFailed {
        stderr: e.message().to_string(),
    };
    let mut remote =
        git2::Remote::create_detached(url).map_err(|e| SourceError::GitCommand(e.to_string()))?;
    let conn = remote
        .connect_auth(Direction::Fetch, None, None)
        .map_err(git_error)?;
    let list = conn.list().map_err(git_error)?;

    branch_head(&advertised(list), branch)
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
        Some(name) => {
            tip_of(refs, branch_key::branch(name)).ok_or_else(|| SourceError::BranchNotFound {
                branch: name.to_owned(),
            })
        }
        None => default_branch_head(refs).ok_or(SourceError::EmptyRemote),
    }
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
