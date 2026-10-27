/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::ssh::with_private_key;
use super::url::{git_transport_url, set_shallow_unless_local};
use anyhow::{Context, Result};
use git2::{Direction, FetchOptions, Oid, Remote, Repository};
use tempfile::TempDir;
use tracing::debug;

pub fn checkout_commit(url: &str, commit: &str, ssh_key: Option<&str>) -> Result<TempDir> {
    with_private_key(ssh_key, || {
        checkout_into_temp_dir(git_transport_url(url), commit)
    })
}

fn checkout_into_temp_dir(url: &str, commit: &str) -> Result<TempDir> {
    let oid = Oid::from_str(commit).with_context(|| format!("invalid commit SHA: {commit}"))?;
    let checkout = tempfile::Builder::new()
        .prefix("gradient-fetch-")
        .tempdir()
        .context("create the checkout directory")?;
    let repo = Repository::init(checkout.path()).context("init the checkout")?;

    Origin::new(&repo, url)?
        .fetch_commit(oid)
        .with_context(|| format!("fetch commit {commit} from {url}"))?;

    let pinned = repo
        .find_commit(oid)
        .with_context(|| format!("commit {commit} not in the fetch from {url}"))?;
    let mut options = git2::build::CheckoutBuilder::new();
    options.force();
    repo.checkout_tree(pinned.as_object(), Some(&mut options))
        .context("check out the tree")?;

    Ok(checkout)
}

struct Origin<'a> {
    remote: Remote<'a>,
    url: &'a str,
}

impl<'a> Origin<'a> {
    fn new(repo: &'a Repository, url: &'a str) -> Result<Self> {
        let remote = repo
            .remote_anonymous(url)
            .with_context(|| format!("invalid repository URL {url}"))?;
        Ok(Self { remote, url })
    }

    fn fetch_commit(&mut self, oid: Oid) -> Result<(), git2::Error> {
        if let Some(tip) = self.ref_pointing_at(oid)? {
            return self.fetch(&tip, true);
        }
        let Err(e) = self.fetch(&oid.to_string(), true) else {
            return Ok(());
        };
        debug!(error = %e, url = self.url, "fetch by commit hash refused, fetching the branches");
        self.fetch("+refs/heads/*:refs/remotes/origin/*", false)
    }

    fn ref_pointing_at(&mut self, oid: Oid) -> Result<Option<String>, git2::Error> {
        let connection = self.remote.connect_auth(Direction::Fetch, None, None)?;
        Ok(connection
            .list()?
            .iter()
            .find(|head| head.oid() == oid && !head.name().ends_with("^{}"))
            .map(|head| head.name().to_owned()))
    }

    fn fetch(&mut self, refspec: &str, shallow: bool) -> Result<(), git2::Error> {
        let mut options = FetchOptions::new();
        if shallow {
            set_shallow_unless_local(&mut options, self.url);
        }
        self.remote.fetch(&[refspec], Some(&mut options), None)
    }
}
