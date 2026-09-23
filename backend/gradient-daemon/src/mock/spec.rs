/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonConfig {
    pub worker: String,
    pub timing: Timing,
    pub derivations: BTreeMap<String, Node>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub name: String,
    pub drv_path: String,
    pub fixed_output: bool,
    pub fod_content: Option<String>,
    pub outputs: BTreeMap<String, Output>,
    pub build: Build,
    pub present: Present,
    pub timing: Timing,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Output {
    pub path: String,
    pub size: u64,
    pub references: Vec<String>,
    pub products: Vec<Product>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Product {
    #[serde(rename = "type")]
    pub kind: String,
    pub subtype: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Build {
    pub duration_ms: Option<u64>,
    pub outcome: Outcome,
    pub fail_status: String,
    pub log: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Success,
    Fail,
    Hang,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Present {
    pub workers: Vec<String>,
    pub cache: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Timing {
    pub seed: u64,
    pub scale: f64,
    pub build_ms: Dist,
    pub chunk_ms: Dist,
    pub chunk_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "dist", rename_all = "lowercase")]
pub enum Dist {
    Lognormal { median: f64, p99: f64 },
    Uniform { min: f64, max: f64 },
    Fixed { ms: f64 },
}

impl DaemonConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn by_drv(&self, drv: &str) -> Option<(&str, &Node)> {
        self.derivations
            .iter()
            .find(|(_, n)| n.drv_path == drv)
            .map(|(id, n)| (id.as_str(), n))
    }

    pub fn by_output(&self, path: &str) -> Option<(&str, &Node, &str)> {
        self.derivations.iter().find_map(|(id, n)| {
            n.outputs
                .iter()
                .find(|(_, o)| o.path == path)
                .map(|(name, _)| (id.as_str(), n, name.as_str()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn fixture() -> DaemonConfig {
        DaemonConfig::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config.json"),
        )
        .expect("fixture parses")
    }

    #[test]
    fn looks_up_nodes_by_drv_and_output() {
        let config = fixture();
        let app_drv = config.derivations["t/app"].drv_path.clone();
        let lib_out = config.derivations["t/lib"].outputs["out"].path.clone();
        let (id, node) = config.by_drv(&app_drv).expect("app");
        assert_eq!((id, node.name.as_str()), ("t/app", "app"));
        let (id, _, output) = config.by_output(&lib_out).expect("lib out");
        assert_eq!((id, output), ("t/lib", "out"));
    }

    #[test]
    fn outcome_and_dist_parse() {
        let config = fixture();
        assert_eq!(config.derivations["t/lib"].build.outcome, Outcome::Success);
        assert!(matches!(
            config.derivations["t/app"].timing.build_ms,
            Dist::Lognormal { .. }
        ));
    }
}
