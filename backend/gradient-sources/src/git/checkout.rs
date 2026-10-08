/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::remote::fetch_options_with_ssh;
use super::url::{git_transport_url, set_shallow_unless_local};
use anyhow::{Context, Result};
use git2::{Oid, Repository};
use tempfile::TempDir;

pub fn checkout_commit(url: &str, commit: &str, ssh_key: Option<&str>) -> Result<TempDir> {
    let url = git_transport_url(url);
    let oid = Oid::from_str(commit).with_context(|| format!("invalid commit SHA: {commit}"))?;
    let checkout = tempfile::Builder::new()
        .prefix("gradient-fetch-")
        .tempdir()
        .context("failed to create the checkout directory")?;
    let repo = Repository::init(checkout.path()).context("failed to init the checkout")?;

    let mut fetch = fetch_options_with_ssh(ssh_key);
    set_shallow_unless_local(&mut fetch, url);
    repo.remote_anonymous(url)
        .with_context(|| format!("invalid repository URL {url}"))?
        .fetch(&[commit], Some(&mut fetch), None)
        .with_context(|| format!("failed to fetch commit {commit} from {url}"))?;

    let pinned = repo
        .find_commit(oid)
        .with_context(|| format!("commit {commit} not in the fetch from {url}"))?;
    let mut options = git2::build::CheckoutBuilder::new();
    options.force();
    repo.checkout_tree(pinned.as_object(), Some(&mut options))
        .context("checkout failed")?;

    Ok(checkout)
}
