/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The [`PatchGenerator`] trait and the native [`FlakeLockGenerator`].
//!
//! The generator owns the pure lock-rewrite logic and delegates all I/O
//! (revision lookup + narHash) to a [`RevisionResolver`], so the rewrite is
//! unit-tested with a fake resolver and no network or nix.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::lock::FlakeLock;
use crate::resolver::RevisionResolver;

/// A tracked flake input name (a key in the root node's `inputs`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InputName(pub String);

impl From<&str> for InputName {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for InputName {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for InputName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One input that the patch bumped, for templating and the sidecar row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BumpedInput {
    pub name: String,
    pub old_rev: Option<String>,
    pub new_rev: String,
}

/// A single file edit produced by a generator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEdit {
    pub path: PathBuf,
    pub contents: Vec<u8>,
}

/// The output of a generator: file edits plus the set of bumped inputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Patch {
    pub edits: Vec<FileEdit>,
    pub bumped: Vec<BumpedInput>,
}

/// Produces a [`Patch`] from a checkout. v1 has a single impl,
/// [`FlakeLockGenerator`]; a future `UpdateScriptGenerator` slots in here.
#[async_trait]
pub trait PatchGenerator: Send + Sync {
    /// Produce a patch bumping `tracked` inputs, or `None` when nothing changed.
    async fn produce(&self, checkout: &Path, tracked: &[InputName]) -> Result<Option<Patch>>;
}

/// Native zero-nix `flake.lock` updater.
#[derive(Debug)]
pub struct FlakeLockGenerator<R: RevisionResolver> {
    resolver: R,
}

impl<R: RevisionResolver> FlakeLockGenerator<R> {
    pub fn new(resolver: R) -> Self {
        Self { resolver }
    }
}

#[async_trait]
impl<R: RevisionResolver> PatchGenerator for FlakeLockGenerator<R> {
    async fn produce(&self, checkout: &Path, tracked: &[InputName]) -> Result<Option<Patch>> {
        let lock_path = checkout.join("flake.lock");
        let bytes = tokio::fs::read(&lock_path)
            .await
            .with_context(|| format!("reading {}", lock_path.display()))?;
        let mut lock = FlakeLock::parse(&bytes)?;

        let declared = lock.root_input_names();
        let mut targets = Vec::with_capacity(tracked.len());
        let mut seen = std::collections::BTreeSet::new();
        for input in tracked {
            let names: Vec<String> = if gradient_util::glob::is_pattern(&input.0) {
                declared
                    .iter()
                    .filter(|d| gradient_util::glob::glob_match(&input.0, d))
                    .cloned()
                    .collect()
            } else {
                vec![input.0.clone()]
            };
            for name in names {
                if !seen.insert(name.clone()) {
                    continue;
                }
                let node_name = lock
                    .input_node_name(&name)
                    .with_context(|| format!("tracked input `{name}` is not a direct flake input"))?
                    .to_owned();
                targets.push((name, node_name));
            }
        }

        let mut resolutions = Vec::with_capacity(targets.len());
        for (input, node_name) in &targets {
            let node = lock
                .nodes
                .get(node_name)
                .with_context(|| format!("lock node `{node_name}` missing for input `{input}`"))?;
            let reference = node
                .original_ref()
                .with_context(|| format!("reading `original` of input `{input}`"))?;
            let old_rev = node.locked_rev().map(str::to_owned);
            let resolved = self
                .resolver
                .resolve(&reference)
                .await
                .with_context(|| format!("resolving newest revision for input `{input}`"))?;
            resolutions.push((
                input.clone(),
                node_name.clone(),
                old_rev,
                reference,
                resolved,
            ));
        }

        let mut bumped = Vec::new();
        for (input, node_name, old_rev, reference, resolved) in resolutions {
            if old_rev.as_deref() == Some(resolved.rev.as_str()) {
                continue;
            }

            let node = lock
                .nodes
                .get_mut(&node_name)
                .with_context(|| format!("lock node `{node_name}` missing for input `{input}`"))?;
            let locked = node.locked.get_or_insert_with(Default::default);
            locked.insert("rev".into(), Value::from(resolved.rev.clone()));
            locked.insert("narHash".into(), Value::from(resolved.nar_hash));
            locked.insert("lastModified".into(), Value::from(resolved.last_modified));

            match resolved.ref_.filter(|_| reference.locked_keeps_ref()) {
                Some(r) => locked.insert("ref".into(), Value::from(r)),
                None => locked.remove("ref"),
            };

            bumped.push(BumpedInput {
                name: input,
                old_rev,
                new_rev: resolved.rev,
            });
        }

        if bumped.is_empty() {
            return Ok(None);
        }

        let contents = lock.to_bytes()?;

        Ok(Some(Patch {
            edits: vec![FileEdit {
                path: PathBuf::from("flake.lock"),
                contents,
            }],
            bumped,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::LockedRef;
    use crate::resolver::ResolvedRev;
    use std::collections::HashMap;

    struct FakeResolver(HashMap<String, ResolvedRev>);

    #[async_trait]
    impl RevisionResolver for FakeResolver {
        async fn resolve(&self, reference: &LockedRef) -> Result<ResolvedRev> {
            let key = match reference {
                LockedRef::Github { repo, .. } => repo.clone(),
                LockedRef::Git { url, .. } => url.clone(),
                _ => anyhow::bail!("unexpected ref in test"),
            };
            self.0.get(&key).cloned().context("no canned resolution")
        }
    }

    fn write_fixture(dir: &Path, rev: &str) {
        let lock = format!(
            r#"{{
  "nodes": {{
    "nixpkgs": {{
      "locked": {{
        "lastModified": 1700000000,
        "narHash": "sha256-OLD0000000000000000000000000000000000000000=",
        "owner": "NixOS",
        "repo": "nixpkgs",
        "rev": "{rev}",
        "type": "github"
      }},
      "original": {{
        "owner": "NixOS",
        "ref": "nixos-unstable",
        "repo": "nixpkgs",
        "type": "github"
      }}
    }},
    "root": {{ "inputs": {{ "nixpkgs": "nixpkgs" }} }}
  }},
  "root": "root",
  "version": 7
}}
"#
        );
        std::fs::write(dir.join("flake.lock"), lock).unwrap();
    }

    #[tokio::test]
    async fn bumps_changed_input() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture(dir.path(), "1111111111111111111111111111111111111111");

        let resolved = ResolvedRev {
            rev: "2222222222222222222222222222222222222222".into(),
            ref_: Some("nixos-unstable".into()),
            nar_hash: "sha256-NEW0000000000000000000000000000000000000000=".into(),
            last_modified: 1800000000,
        };
        let lockgen =
            FlakeLockGenerator::new(FakeResolver(HashMap::from([("nixpkgs".into(), resolved)])));

        let patch = lockgen
            .produce(dir.path(), &["nixpkgs".into()])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(patch.bumped.len(), 1);
        assert_eq!(
            patch.bumped[0].new_rev,
            "2222222222222222222222222222222222222222"
        );

        let out = FlakeLock::parse(&patch.edits[0].contents).unwrap();
        let node = &out.nodes["nixpkgs"];
        assert_eq!(
            node.locked_rev().unwrap(),
            "2222222222222222222222222222222222222222"
        );
        assert_eq!(
            node.locked.as_ref().unwrap()["narHash"].as_str().unwrap(),
            "sha256-NEW0000000000000000000000000000000000000000="
        );
        assert!(
            !node.locked.as_ref().unwrap().contains_key("ref"),
            "github locked blocks must pin by rev alone; a ref makes nix reject the input"
        );
    }

    #[tokio::test]
    async fn glob_target_expands_over_matching_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let node = |repo: &str, rev: &str| {
            format!(
                r#"{{
      "locked": {{ "lastModified": 1700000000, "narHash": "sha256-OLD0000000000000000000000000000000000000000=", "owner": "o", "repo": "{repo}", "rev": "{rev}", "type": "github" }},
      "original": {{ "owner": "o", "repo": "{repo}", "type": "github" }}
    }}"#
            )
        };
        let lock = format!(
            r#"{{
  "nodes": {{
    "nixpkgs": {},
    "nixpkgs-lib": {},
    "flake-utils": {},
    "root": {{ "inputs": {{ "nixpkgs": "nixpkgs", "nixpkgs-lib": "nixpkgs-lib", "flake-utils": "flake-utils" }} }}
  }},
  "root": "root",
  "version": 7
}}
"#,
            node("nixpkgs", "1111111111111111111111111111111111111111"),
            node("nixpkgs-lib", "1111111111111111111111111111111111111111"),
            node("flake-utils", "1111111111111111111111111111111111111111"),
        );
        std::fs::write(dir.path().join("flake.lock"), lock).unwrap();

        let bumped = |rev: &str| ResolvedRev {
            rev: rev.into(),
            ref_: None,
            nar_hash: "sha256-NEW0000000000000000000000000000000000000000=".into(),
            last_modified: 1800000000,
        };
        let lockgen = FlakeLockGenerator::new(FakeResolver(HashMap::from([
            (
                "nixpkgs".into(),
                bumped("2222222222222222222222222222222222222222"),
            ),
            (
                "nixpkgs-lib".into(),
                bumped("3333333333333333333333333333333333333333"),
            ),
        ])));

        let patch = lockgen
            .produce(dir.path(), &["nixpkgs*".into()])
            .await
            .unwrap()
            .unwrap();
        let names: std::collections::BTreeSet<&str> =
            patch.bumped.iter().map(|b| b.name.as_str()).collect();
        assert!(names.contains("nixpkgs"));
        assert!(names.contains("nixpkgs-lib"));
        assert!(!names.contains("flake-utils"));
    }

    #[tokio::test]
    async fn drops_stale_ref_from_github_locked() {
        let dir = tempfile::tempdir().unwrap();
        let poisoned = r#"{
  "nodes": {
    "nixpkgs": {
      "locked": {
        "lastModified": 1700000000,
        "narHash": "sha256-OLD0000000000000000000000000000000000000000=",
        "owner": "NixOS",
        "ref": "nixos-unstable",
        "repo": "nixpkgs",
        "rev": "1111111111111111111111111111111111111111",
        "type": "github"
      },
      "original": {
        "owner": "NixOS",
        "ref": "nixos-unstable",
        "repo": "nixpkgs",
        "type": "github"
      }
    },
    "root": { "inputs": { "nixpkgs": "nixpkgs" } }
  },
  "root": "root",
  "version": 7
}
"#;
        std::fs::write(dir.path().join("flake.lock"), poisoned).unwrap();

        let resolved = ResolvedRev {
            rev: "2222222222222222222222222222222222222222".into(),
            ref_: Some("nixos-unstable".into()),
            nar_hash: "sha256-NEW0000000000000000000000000000000000000000=".into(),
            last_modified: 1800000000,
        };
        let lockgen =
            FlakeLockGenerator::new(FakeResolver(HashMap::from([("nixpkgs".into(), resolved)])));

        let patch = lockgen
            .produce(dir.path(), &["nixpkgs".into()])
            .await
            .unwrap()
            .unwrap();
        let out = FlakeLock::parse(&patch.edits[0].contents).unwrap();
        assert!(
            !out.nodes["nixpkgs"]
                .locked
                .as_ref()
                .unwrap()
                .contains_key("ref"),
            "a previously poisoned github locked block must heal on the next bump"
        );
    }

    #[tokio::test]
    async fn keeps_ref_for_git_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let lock = r#"{
  "nodes": {
    "dep": {
      "locked": {
        "lastModified": 1700000000,
        "narHash": "sha256-OLD0000000000000000000000000000000000000000=",
        "ref": "refs/heads/main",
        "rev": "1111111111111111111111111111111111111111",
        "revCount": 10,
        "type": "git",
        "url": "https://example.com/dep.git"
      },
      "original": {
        "ref": "refs/heads/main",
        "type": "git",
        "url": "https://example.com/dep.git"
      }
    },
    "root": { "inputs": { "dep": "dep" } }
  },
  "root": "root",
  "version": 7
}
"#;
        std::fs::write(dir.path().join("flake.lock"), lock).unwrap();

        let resolved = ResolvedRev {
            rev: "2222222222222222222222222222222222222222".into(),
            ref_: Some("refs/heads/main".into()),
            nar_hash: "sha256-NEW0000000000000000000000000000000000000000=".into(),
            last_modified: 1800000000,
        };
        let lockgen = FlakeLockGenerator::new(FakeResolver(HashMap::from([(
            "https://example.com/dep.git".into(),
            resolved,
        )])));

        let patch = lockgen
            .produce(dir.path(), &["dep".into()])
            .await
            .unwrap()
            .unwrap();
        let out = FlakeLock::parse(&patch.edits[0].contents).unwrap();
        let locked = out.nodes["dep"].locked.as_ref().unwrap();
        assert_eq!(
            locked["rev"].as_str().unwrap(),
            "2222222222222222222222222222222222222222"
        );
        assert_eq!(
            locked["ref"].as_str().unwrap(),
            "refs/heads/main",
            "git locked blocks keep their ref alongside rev"
        );
    }

    #[tokio::test]
    async fn short_circuits_when_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture(dir.path(), "1111111111111111111111111111111111111111");

        let resolved = ResolvedRev {
            rev: "1111111111111111111111111111111111111111".into(),
            ref_: Some("nixos-unstable".into()),
            nar_hash: "sha256-OLD0000000000000000000000000000000000000000=".into(),
            last_modified: 1700000000,
        };
        let lockgen =
            FlakeLockGenerator::new(FakeResolver(HashMap::from([("nixpkgs".into(), resolved)])));

        let patch = lockgen
            .produce(dir.path(), &["nixpkgs".into()])
            .await
            .unwrap();
        assert!(
            patch.is_none(),
            "no rev change must short-circuit to no patch"
        );
    }

    #[tokio::test]
    async fn unknown_input_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture(dir.path(), "1111111111111111111111111111111111111111");

        let lockgen = FlakeLockGenerator::new(FakeResolver(HashMap::new()));
        let err = lockgen
            .produce(dir.path(), &["does-not-exist".into()])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a direct flake input"));
    }
}
