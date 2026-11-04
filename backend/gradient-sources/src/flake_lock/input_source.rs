/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::lock::{FlakeLock, LockedRef};
use gradient_util::glob::{glob_match, is_pattern};
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputSource {
    pub name: String,
    pub repository_url: String,
    pub branch: Option<String>,
}

pub fn tracked_input_sources(lock: &FlakeLock, tracked: &[String]) -> Vec<InputSource> {
    lock.root_input_names()
        .into_iter()
        .filter(|name| is_tracked(name, tracked))
        .filter_map(|name| {
            let node = lock.nodes.get(lock.input_node_name(&name)?)?;
            let (repository_url, branch) = repository_of(node.original.as_ref()?)?;
            Some(InputSource {
                name,
                repository_url,
                branch,
            })
        })
        .collect()
}

fn is_tracked(name: &str, tracked: &[String]) -> bool {
    tracked
        .iter()
        .any(|t| t == name || (is_pattern(t) && glob_match(t, name)))
}

fn repository_of(original: &Map<String, Value>) -> Option<(String, Option<String>)> {
    let host = |default: &str| {
        original
            .get("host")
            .and_then(Value::as_str)
            .unwrap_or(default)
            .to_owned()
    };
    let (url, branch) = match LockedRef::from_map(original).ok()? {
        LockedRef::Github { owner, repo, ref_ } => (
            format!("https://{}/{owner}/{repo}", host("github.com")),
            ref_,
        ),
        LockedRef::Gitlab { owner, repo, ref_ } => (
            format!(
                "https://{}/{}/{repo}",
                host("gitlab.com"),
                owner.replace("%2F", "/")
            ),
            ref_,
        ),
        LockedRef::Sourcehut { owner, repo, ref_ } => (
            format!("https://{}/{owner}/{repo}", host("git.sr.ht")),
            ref_,
        ),
        LockedRef::Git { url, ref_ } => (url, ref_),
        _ => return None,
    };
    let branch = branch.map(|b| b.strip_prefix("refs/heads/").unwrap_or(&b).to_owned());
    Some((url, branch))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &[u8] = br#"{
  "nodes": {
    "nixpkgs": {
      "locked": { "owner": "NixOS", "repo": "nixpkgs", "rev": "1111111111111111111111111111111111111111", "type": "github" },
      "original": { "owner": "dennis", "ref": "nixos-unstable", "repo": "nixpkgs", "type": "github" }
    },
    "home-manager": {
      "locked": { "owner": "nix-community", "repo": "home-manager", "rev": "2222222222222222222222222222222222222222", "type": "github" },
      "original": { "owner": "nix-community", "repo": "home-manager", "type": "github" }
    },
    "private": {
      "locked": { "rev": "3333333333333333333333333333333333333333", "type": "git", "url": "ssh://git@git.example.com/team/private.git" },
      "original": { "ref": "refs/heads/stable", "type": "git", "url": "ssh://git@git.example.com/team/private.git" }
    },
    "nested": {
      "locked": { "owner": "group/sub", "repo": "tool", "rev": "4444444444444444444444444444444444444444", "type": "gitlab" },
      "original": { "host": "gitlab.example.com", "owner": "group%2Fsub", "repo": "tool", "type": "gitlab" }
    },
    "archive": {
      "locked": { "type": "tarball", "url": "https://example.com/a.tar.gz" },
      "original": { "type": "tarball", "url": "https://example.com/a.tar.gz" }
    },
    "root": {
      "inputs": {
        "archive": "archive",
        "home-manager": "home-manager",
        "nested": "nested",
        "nixpkgs": "nixpkgs",
        "private": "private",
        "follower": ["nixpkgs"]
      }
    }
  },
  "root": "root",
  "version": 7
}
"#;

    fn sources(tracked: &[&str]) -> Vec<InputSource> {
        let lock = FlakeLock::parse(LOCK).unwrap();
        let tracked: Vec<String> = tracked.iter().map(|t| (*t).to_owned()).collect();
        tracked_input_sources(&lock, &tracked)
    }

    fn source(name: &str, url: &str, branch: Option<&str>) -> InputSource {
        InputSource {
            name: name.into(),
            repository_url: url.into(),
            branch: branch.map(Into::into),
        }
    }

    #[test]
    fn a_github_input_names_its_fork_and_branch() {
        assert_eq!(
            sources(&["nixpkgs"]),
            vec![source(
                "nixpkgs",
                "https://github.com/dennis/nixpkgs",
                Some("nixos-unstable")
            )]
        );
    }

    #[test]
    fn a_glob_expands_and_an_input_without_ref_has_no_branch() {
        assert_eq!(
            sources(&["home-*", "nixpkgs"]),
            vec![
                source(
                    "home-manager",
                    "https://github.com/nix-community/home-manager",
                    None
                ),
                source(
                    "nixpkgs",
                    "https://github.com/dennis/nixpkgs",
                    Some("nixos-unstable")
                ),
            ]
        );
    }

    #[test]
    fn git_and_self_hosted_gitlab_inputs_keep_their_host() {
        assert_eq!(
            sources(&["private", "nested"]),
            vec![
                source("nested", "https://gitlab.example.com/group/sub/tool", None),
                source(
                    "private",
                    "ssh://git@git.example.com/team/private.git",
                    Some("stable")
                ),
            ]
        );
    }

    #[test]
    fn tarballs_and_follows_have_no_repository() {
        assert!(sources(&["archive", "follower"]).is_empty());
    }
}
