/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Lossless typed model of a `flake.lock` (schema version 7).
//!
//! `locked`/`original` blocks are kept as sorted `serde_json::Map`s so unknown
//! keys survive a round-trip and re-serialization is byte-stable against nix's
//! nlohmann output (sorted keys, 2-space indent). A [`LockedRef`] *view* is
//! parsed on demand to drive resolution.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The whole `flake.lock` document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlakeLock {
    pub nodes: BTreeMap<String, Node>,
    pub root: String,
    pub version: u64,
}

/// A single lock node. Fields are declared alphabetically so serde emits them
/// in the same order nix's sorted-key serializer does.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flake: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, InputRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<Map<String, Value>>,
}

/// An entry in a node's `inputs`: either a direct node name or a `follows` path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputRef {
    Direct(String),
    Follows(Vec<String>),
}

/// Typed view over a `locked`/`original` block, keyed on its `type`. Drives
/// revision resolution. Unsupported types parse into [`LockedRef::Other`] and
/// fail explicitly at resolution time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockedRef {
    Github { owner: String, repo: String, ref_: Option<String> },
    Gitlab { owner: String, repo: String, ref_: Option<String> },
    Sourcehut { owner: String, repo: String, ref_: Option<String> },
    Git { url: String, ref_: Option<String> },
    Tarball { url: String },
    Path { path: String },
    Indirect { id: String },
    Other(String),
}

impl FlakeLock {
    /// Parse a `flake.lock` byte buffer.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let lock: FlakeLock = serde_json::from_slice(bytes).context("parsing flake.lock")?;
        if lock.version != 7 {
            bail!("unsupported flake.lock version {} (only 7 is supported)", lock.version);
        }

        Ok(lock)
    }

    /// Serialize to the canonical nix layout: 2-space indent, sorted keys,
    /// trailing newline.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut out = serde_json::to_vec_pretty(self).context("serializing flake.lock")?;
        out.push(b'\n');

        Ok(out)
    }

    /// The node a root input name points at, following one level of indirection
    /// through the root node's `inputs` map.
    pub fn input_node_name(&self, input: &str) -> Option<&str> {
        match self.nodes.get(&self.root)?.inputs.get(input)? {
            InputRef::Direct(name) => Some(name),
            InputRef::Follows(_) => None,
        }
    }
}

impl Node {
    /// Current locked revision, if this node has one.
    pub fn locked_rev(&self) -> Option<&str> {
        self.locked.as_ref()?.get("rev")?.as_str()
    }

    /// Parse the `original` block into a typed [`LockedRef`].
    pub fn original_ref(&self) -> Result<LockedRef> {
        let map = self.original.as_ref().context("node has no `original` block")?;
        LockedRef::from_map(map)
    }
}

impl LockedRef {
    /// Parse a typed reference from a `locked`/`original` block.
    pub fn from_map(map: &Map<String, Value>) -> Result<Self> {
        let ty = map.get("type").and_then(Value::as_str).context("ref has no `type`")?;
        let s = |k: &str| map.get(k).and_then(Value::as_str).map(str::to_owned);
        let req = |k: &str| s(k).with_context(|| format!("{ty} ref missing `{k}`"));

        Ok(match ty {
            "github" => Self::Github { owner: req("owner")?, repo: req("repo")?, ref_: s("ref") },
            "gitlab" => Self::Gitlab { owner: req("owner")?, repo: req("repo")?, ref_: s("ref") },
            "sourcehut" => {
                Self::Sourcehut { owner: req("owner")?, repo: req("repo")?, ref_: s("ref") }
            }
            "git" => Self::Git { url: req("url")?, ref_: s("ref") },
            "tarball" => Self::Tarball { url: req("url")? },
            "path" => Self::Path { path: req("path")? },
            "indirect" => Self::Indirect { id: req("id")? },
            other => Self::Other(other.to_owned()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = br#"{
  "nodes": {
    "nixpkgs": {
      "locked": {
        "lastModified": 1700000000,
        "narHash": "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        "owner": "NixOS",
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
    "root": {
      "inputs": {
        "nixpkgs": "nixpkgs"
      }
    }
  },
  "root": "root",
  "version": 7
}
"#;

    #[test]
    fn parses_and_round_trips_byte_stable() {
        let lock = FlakeLock::parse(FIXTURE).unwrap();
        let bytes = lock.to_bytes().unwrap();
        assert_eq!(bytes, FIXTURE, "serialization must be byte-identical to the fixture");

        let again = FlakeLock::parse(&bytes).unwrap();
        assert_eq!(lock, again, "re-parse must be structurally equal");
    }

    #[test]
    fn serialization_is_idempotent() {
        let lock = FlakeLock::parse(FIXTURE).unwrap();
        let once = lock.to_bytes().unwrap();
        let twice = FlakeLock::parse(&once).unwrap().to_bytes().unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn resolves_input_node_and_original_ref() {
        let lock = FlakeLock::parse(FIXTURE).unwrap();
        let node_name = lock.input_node_name("nixpkgs").unwrap();
        assert_eq!(node_name, "nixpkgs");

        let node = &lock.nodes[node_name];
        assert_eq!(node.locked_rev().unwrap(), "1111111111111111111111111111111111111111");
        assert_eq!(
            node.original_ref().unwrap(),
            LockedRef::Github {
                owner: "NixOS".into(),
                repo: "nixpkgs".into(),
                ref_: Some("nixos-unstable".into()),
            }
        );
    }

    #[test]
    fn rejects_non_v7() {
        let bytes = br#"{"nodes":{},"root":"root","version":6}"#;
        assert!(FlakeLock::parse(bytes).is_err());
    }

    #[test]
    fn follows_input_ref_parses() {
        let bytes = br#"{
  "nodes": {
    "root": {
      "inputs": {
        "a": "a",
        "b": [
          "a"
        ]
      }
    },
    "a": {
      "locked": {},
      "original": {}
    }
  },
  "root": "root",
  "version": 7
}
"#;
        let lock = FlakeLock::parse(bytes).unwrap();
        let root = &lock.nodes["root"];
        assert_eq!(root.inputs["a"], InputRef::Direct("a".into()));
        assert_eq!(root.inputs["b"], InputRef::Follows(vec!["a".into()]));
    }
}
