/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The tree at the pinned commit is serialised as a NAR exactly like nix's git fetcher.
//! It is added to the store as `<hash>-source` without a nix process.

use anyhow::{Context, Result};
use git2::{ObjectType, Oid, Repository, Tree};
use gradient_wire::traits::WorkerStore;

#[tracing::instrument(level = "debug", skip_all)]
pub(crate) async fn add_git_tree(
    store: &dyn WorkerStore,
    repo_path: &str,
    commit: &str,
) -> Result<String> {
    let repo_path = repo_path.to_owned();
    let commit = commit.to_owned();
    let nar = tokio::task::spawn_blocking(move || nar_of_commit(&repo_path, &commit))
        .await
        .context("git export panicked")??;

    store.add_nar("source", nar).await
}

pub(crate) fn nar_of_commit(repo_path: &str, commit: &str) -> Result<Vec<u8>> {
    let repo = Repository::open(repo_path).with_context(|| format!("open {repo_path}"))?;
    let oid = Oid::from_str(commit).with_context(|| format!("invalid commit SHA: {commit}"))?;
    let tree = repo
        .find_commit(oid)
        .and_then(|c| c.tree())
        .with_context(|| format!("tree of {commit} in {repo_path}"))?;

    let mut nar = Vec::new();
    string(&mut nar, b"nix-archive-1");
    directory(&repo, &tree, &mut nar)?;

    Ok(nar)
}

/// NAR entries are in byte order of their names. Git is ordering a subtree as if its
/// name ended in `/`. A submodule is an empty directory, matching nix without
/// `submodules=true`.
fn directory(repo: &Repository, tree: &Tree<'_>, nar: &mut Vec<u8>) -> Result<()> {
    let mut entries: Vec<_> = tree.iter().collect();
    entries.sort_by(|a, b| a.name_bytes().cmp(b.name_bytes()));

    string(nar, b"(");
    string(nar, b"type");
    string(nar, b"directory");
    for entry in entries {
        string(nar, b"entry");
        string(nar, b"(");
        string(nar, b"name");
        string(nar, entry.name_bytes());
        string(nar, b"node");
        match entry.kind() {
            Some(ObjectType::Tree) => directory(repo, &repo.find_tree(entry.id())?, nar)?,
            Some(ObjectType::Blob) => {
                blob(repo.find_blob(entry.id())?.content(), entry.filemode(), nar)
            }
            Some(ObjectType::Commit) => {
                string(nar, b"(");
                string(nar, b"type");
                string(nar, b"directory");
                string(nar, b")");
            }
            other => anyhow::bail!(
                "unexpected tree entry {other:?} at {}",
                String::from_utf8_lossy(entry.name_bytes())
            ),
        }
        string(nar, b")");
    }
    string(nar, b")");

    Ok(())
}

fn blob(content: &[u8], mode: i32, nar: &mut Vec<u8>) {
    string(nar, b"(");
    string(nar, b"type");
    if mode == 0o120000 {
        string(nar, b"symlink");
        string(nar, b"target");
        string(nar, content);
    } else {
        string(nar, b"regular");
        if mode & 0o111 != 0 {
            string(nar, b"executable");
            string(nar, b"");
        }
        string(nar, b"contents");
        string(nar, content);
    }
    string(nar, b")");
}

fn string(nar: &mut Vec<u8>, bytes: &[u8]) {
    nar.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    nar.extend_from_slice(bytes);
    nar.resize(nar.len() + (8 - bytes.len() % 8) % 8, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest as _, Sha256};
    use std::os::unix::fs::PermissionsExt as _;
    use std::process::Command;

    fn git(repo: &std::path::Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn head(repo: &std::path::Path) -> String {
        let out = Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::create_dir_all(repo.join("a")).unwrap();
        std::fs::create_dir_all(repo.join("sub/deep")).unwrap();
        std::fs::write(repo.join("flake.nix"), "{ outputs = _: {}; }\n").unwrap();
        std::fs::write(repo.join("a/file.txt"), "hello\n").unwrap();
        std::fs::write(repo.join("a-b"), "x").unwrap();
        std::fs::write(repo.join("run.sh"), "#!/bin/sh\necho hi\n").unwrap();
        std::fs::set_permissions(repo.join("run.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::os::unix::fs::symlink("a/file.txt", repo.join("link")).unwrap();
        std::fs::write(repo.join("sub/deep/empty"), "").unwrap();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", "init"]);

        dir
    }

    #[test]
    fn the_export_hashes_to_what_nix_fetches_for_the_same_commit() {
        let dir = fixture();
        let nar = nar_of_commit(dir.path().to_str().unwrap(), &head(dir.path())).unwrap();
        assert_eq!(nar.len(), 1824);
        assert_eq!(
            sha256_hex(&nar),
            "b4bb697e62a8154b2ad81776bf425117d20aa2558b211dcf8044edcbed41bc6e"
        );
    }

    #[test]
    fn a_submodule_is_an_empty_directory() {
        let dir = fixture();
        let first = head(dir.path());
        git(
            dir.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{first},vendor/lib"),
            ],
        );
        git(dir.path(), &["commit", "-q", "-m", "gitlink"]);
        let nar = nar_of_commit(dir.path().to_str().unwrap(), &head(dir.path())).unwrap();
        assert_eq!(nar.len(), 2160);
        assert_eq!(
            sha256_hex(&nar),
            "1bac9e1ddfd99cdc279288801613d4ac90a302bcb6877058d4098a5ae1fe8e7d"
        );
    }

    #[test]
    fn a_nar_string_is_length_prefixed_and_padded_to_eight() {
        let mut nar = Vec::new();
        string(&mut nar, b"regular");
        assert_eq!(
            nar,
            [
                7, 0, 0, 0, 0, 0, 0, 0, b'r', b'e', b'g', b'u', b'l', b'a', b'r', 0
            ]
        );
        string(&mut nar, b"");
        assert_eq!(&nar[16..], [0; 8]);
    }
}
