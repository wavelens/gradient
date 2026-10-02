/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlakeLock {
    pub nodes: BTreeMap<String, Node>,
    pub root: String,
    pub version: u64,
}

/// Fields are declared alphabetically. Serde is then emitting them in the order of nix's sorted-key
/// serializer.
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputRef {
    Direct(String),
    Follows(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockedRef {
    Github {
        owner: String,
        repo: String,
        ref_: Option<String>,
    },
    Gitlab {
        owner: String,
        repo: String,
        ref_: Option<String>,
    },
    Sourcehut {
        owner: String,
        repo: String,
        ref_: Option<String>,
    },
    Git {
        url: String,
        ref_: Option<String>,
    },
    Tarball {
        url: String,
    },
    Path {
        path: String,
    },
    Indirect {
        id: String,
    },
    Other(String),
}

impl FlakeLock {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let lock: FlakeLock = serde_json::from_slice(bytes).context("parsing flake.lock")?;
        if lock.version != 7 {
            bail!(
                "unsupported flake.lock version {} (only 7 is supported)",
                lock.version
            );
        }

        Ok(lock)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut out = serde_json::to_vec_pretty(self).context("serializing flake.lock")?;
        out.push(b'\n');

        Ok(out)
    }

    pub fn input_node_name(&self, input: &str) -> Option<&str> {
        match self.nodes.get(&self.root)?.inputs.get(input)? {
            InputRef::Direct(name) => Some(name),
            InputRef::Follows(_) => None,
        }
    }

    pub fn root_input_names(&self) -> Vec<String> {
        self.nodes
            .get(&self.root)
            .map(|r| r.inputs.keys().cloned().collect())
            .unwrap_or_default()
    }
}

impl Node {
    pub fn locked_rev(&self) -> Option<&str> {
        self.locked.as_ref()?.get("rev")?.as_str()
    }

    pub fn original_ref(&self) -> Result<LockedRef> {
        let map = self
            .original
            .as_ref()
            .context("node has no `original` block")?;
        LockedRef::from_map(map)
    }
}

impl LockedRef {
    /// The github-family schemes are pinning by `rev` alone and are rejecting a locked node
    /// carrying both. Only plain `git` is keeping its `ref`.
    pub fn locked_keeps_ref(&self) -> bool {
        matches!(self, Self::Git { .. })
    }

    pub fn from_map(map: &Map<String, Value>) -> Result<Self> {
        let ty = map
            .get("type")
            .and_then(Value::as_str)
            .context("ref has no `type`")?;
        let s = |k: &str| map.get(k).and_then(Value::as_str).map(str::to_owned);
        let req = |k: &str| s(k).with_context(|| format!("{ty} ref missing `{k}`"));

        Ok(match ty {
            "github" => Self::Github {
                owner: req("owner")?,
                repo: req("repo")?,
                ref_: s("ref"),
            },
            "gitlab" => Self::Gitlab {
                owner: req("owner")?,
                repo: req("repo")?,
                ref_: s("ref"),
            },
            "sourcehut" => Self::Sourcehut {
                owner: req("owner")?,
                repo: req("repo")?,
                ref_: s("ref"),
            },
            "git" => Self::Git {
                url: req("url")?,
                ref_: s("ref"),
            },
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
        assert_eq!(
            bytes, FIXTURE,
            "serialization must be byte-identical to the fixture"
        );

        let again = FlakeLock::parse(&bytes).unwrap();
        assert_eq!(lock, again, "re-parse must be structurally equal");
    }

    #[test]
    fn resolves_input_node_and_original_ref() {
        let lock = FlakeLock::parse(FIXTURE).unwrap();
        let node_name = lock.input_node_name("nixpkgs").unwrap();
        assert_eq!(node_name, "nixpkgs");

        let node = &lock.nodes[node_name];
        assert_eq!(
            node.locked_rev().unwrap(),
            "1111111111111111111111111111111111111111"
        );
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
