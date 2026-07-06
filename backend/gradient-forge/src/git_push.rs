/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Authenticated `git push --force` of a single commit. Forges whose REST API
//! cannot force-update a ref (Gitea/Forgejo, GitLab) use this so the PR branch
//! is always one clean commit on the current base, matching the native
//! force-push the GitHub git-refs path performs.

use crate::BranchCommit;
use anyhow::{Context, Result};
use git2::{FetchOptions, Oid, PushOptions, RemoteCallbacks, Repository, Signature, Tree};

/// Reset `head` to a single commit that applies `commit.files` on top of the
/// remote's current `base`, force-pushing over whatever was there. Returns the
/// new commit sha. `cred_user`/`cred_pass` are the HTTPS basic-auth pair the
/// forge accepts for the integration token.
pub async fn force_push_lock_commit(
    git_url: String,
    cred_user: String,
    cred_pass: String,
    head: String,
    base: String,
    commit: BranchCommit,
) -> Result<String> {
    tokio::task::spawn_blocking(move || {
        force_push_blocking(&git_url, &cred_user, &cred_pass, &head, &base, &commit)
    })
    .await
    .context("git push task panicked")?
}

fn force_push_blocking(
    git_url: &str,
    cred_user: &str,
    cred_pass: &str,
    head: &str,
    base: &str,
    commit: &BranchCommit,
) -> Result<String> {
    let dir = tempfile::tempdir().context("git push tempdir")?;
    let repo = Repository::init_bare(dir.path()).context("init bare repo")?;
    let mut remote = repo.remote_anonymous(git_url).context("anonymous remote")?;

    let mut fetch = FetchOptions::new();
    fetch.depth(1).remote_callbacks(credentials_cb(cred_user, cred_pass));
    remote
        .fetch(&[&format!("refs/heads/{base}")], Some(&mut fetch), None)
        .with_context(|| format!("fetching base branch {base}"))?;

    let base_commit = repo
        .find_reference("FETCH_HEAD")
        .and_then(|r| r.peel_to_commit())
        .context("resolving base commit")?;

    let mut tree = base_commit.tree().context("base tree")?;
    for file in &commit.files {
        let blob = repo.blob(&file.contents).context("writing blob")?;
        let oid = upsert_path(&repo, Some(&tree), &file.path, blob)?;
        tree = repo.find_tree(oid).context("reloading tree")?;
    }

    let author = commit
        .author
        .as_ref()
        .context("force-push commit requires a resolved author identity")?;
    let sig = Signature::now(&author.name, &author.email).context("commit signature")?;
    let new_commit = repo
        .commit(None, &sig, &sig, &commit.message, &tree, &[&base_commit])
        .context("creating commit")?;
    repo.reference(&format!("refs/heads/{head}"), new_commit, true, "flake.lock update")
        .context("local head ref")?;

    let mut push = PushOptions::new();
    push.remote_callbacks(credentials_cb(cred_user, cred_pass));
    remote
        .push(&[&format!("+refs/heads/{head}:refs/heads/{head}")], Some(&mut push))
        .with_context(|| format!("force-pushing {head}"))?;

    Ok(new_commit.to_string())
}

fn credentials_cb<'a>(user: &'a str, pass: &'a str) -> RemoteCallbacks<'a> {
    let mut cb = RemoteCallbacks::new();
    cb.credentials(move |_url, _username, _allowed| git2::Cred::userpass_plaintext(user, pass));

    cb
}

/// Insert `blob` at `path` into a copy of `base`, rebuilding intermediate
/// trees, and return the new root tree oid.
fn upsert_path(repo: &Repository, base: Option<&Tree>, path: &str, blob: Oid) -> Result<Oid> {
    match path.split_once('/') {
        None => {
            let mut builder = repo.treebuilder(base).context("treebuilder")?;
            builder.insert(path, blob, i32::from(git2::FileMode::Blob)).context("insert blob")?;
            builder.write().context("write tree")
        }
        Some((dir, rest)) => {
            let sub = base
                .and_then(|t| t.get_name(dir))
                .and_then(|e| e.to_object(repo).ok())
                .and_then(|o| o.into_tree().ok());
            let sub_oid = upsert_path(repo, sub.as_ref(), rest, blob)?;
            let mut builder = repo.treebuilder(base).context("treebuilder")?;
            builder.insert(dir, sub_oid, i32::from(git2::FileMode::Tree)).context("insert subtree")?;
            builder.write().context("write tree")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_replaces_file_and_preserves_subtree() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init_bare(dir.path()).unwrap();

        let mut tb = repo.treebuilder(None).unwrap();
        let old = repo.blob(b"old lock").unwrap();
        tb.insert("flake.lock", old, i32::from(git2::FileMode::Blob)).unwrap();
        let base = repo.find_tree(tb.write().unwrap()).unwrap();

        let keep = repo.blob(b"keep me").unwrap();
        let with_dir = repo.find_tree(upsert_path(&repo, Some(&base), "dir/keep.txt", keep).unwrap()).unwrap();

        let new = repo.blob(b"new lock").unwrap();
        let result = repo.find_tree(upsert_path(&repo, Some(&with_dir), "flake.lock", new).unwrap()).unwrap();

        let lock = result.get_name("flake.lock").unwrap().to_object(&repo).unwrap();
        assert_eq!(lock.as_blob().unwrap().content(), b"new lock");

        let nested = result
            .get_name("dir")
            .unwrap()
            .to_object(&repo)
            .unwrap()
            .peel_to_tree()
            .unwrap()
            .get_name("keep.txt")
            .unwrap()
            .to_object(&repo)
            .unwrap();
        assert_eq!(nested.as_blob().unwrap().content(), b"keep me");
    }
}
