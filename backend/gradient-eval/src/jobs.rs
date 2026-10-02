/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use serde::Serialize;

use crate::nix_eval::NixEvaluator;
use crate::nix_store_path;

#[derive(Debug, Serialize)]
pub struct Job {
    pub attr: String,
    #[serde(rename = "attrPath")]
    pub attr_path: Vec<String>,
    #[serde(rename = "drvPath", skip_serializing_if = "Option::is_none")]
    pub drv_path: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Job {
    pub fn resolved(attr: String, drv: String, references: Vec<String>) -> Self {
        Job {
            attr_path: attr.split('.').map(str::to_string).collect(),
            attr,
            drv_path: Some(nix_store_path(&drv)),
            references,
            error: None,
        }
    }

    pub fn failed(attr: String, error: String) -> Self {
        Job {
            attr_path: attr.split('.').map(str::to_string).collect(),
            attr,
            drv_path: None,
            references: vec![],
            error: Some(error),
        }
    }
}

/// Concrete attr paths are skipping the discovery walk. Walking siblings can cost orders of
/// magnitude more for a flake with NixOS VM test `checks`. The function is Boehm-GC bound and must
/// run outside a Tokio runtime.
pub fn eval_jobs(flake_ref: &str, wildcards: &[String], mut sink: impl FnMut(Job)) -> Result<()> {
    let evaluator = NixEvaluator::new()?;
    let walker = evaluator.walker(flake_ref, &[])?;

    let attrs = if wildcards.iter().all(|w| is_concrete_attr(w)) {
        wildcards.to_vec()
    } else {
        walker.discover(wildcards, None)?.0
    };

    for attr in attrs {
        let job = match walker.resolve(&attr) {
            Ok((drv, references)) => Job::resolved(attr, drv, references),
            Err(e) => Job::failed(attr, format!("{e:#}")),
        };
        sink(job);
    }
    let _ = walker.commit_cache();
    Ok(())
}

/// An exclusion is pruning across the whole include set. Its presence must keep every pattern on
/// the discovery path.
fn is_concrete_attr(pattern: &str) -> bool {
    !pattern.starts_with('!') && pattern.split('.').all(|seg| seg != "*" && seg != "#")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_job_serializes_like_nix_eval_jobs() {
        let job = Job::resolved(
            "packages.x86_64-linux.hello".into(),
            "aaaa-hello.drv".into(),
            vec![],
        );
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&job).unwrap()).unwrap();
        assert_eq!(v["attr"], "packages.x86_64-linux.hello");
        assert_eq!(
            v["attrPath"],
            serde_json::json!(["packages", "x86_64-linux", "hello"])
        );
        assert_eq!(v["drvPath"], "/nix/store/aaaa-hello.drv");
        assert!(v.get("error").is_none(), "no error key on success");
        assert!(v.get("references").is_none(), "empty references omitted");
    }

    #[test]
    fn concrete_attrs_skip_discovery_wildcards_do_not() {
        assert!(is_concrete_attr("packages.x86_64-linux.gradient-cli-full"));
        assert!(is_concrete_attr("gradient-cli-full"));
        assert!(!is_concrete_attr("packages.x86_64-linux.*"));
        assert!(!is_concrete_attr("checks.*.*"));
        assert!(!is_concrete_attr("packages.x86_64-linux.#"));
        assert!(!is_concrete_attr("!packages.x86_64-linux.hello"));
    }

    #[test]
    fn failed_job_serializes_error_without_drv_path() {
        let job = Job::failed("packages.x86_64-linux.broken".into(), "boom".into());
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&job).unwrap()).unwrap();
        assert_eq!(v["attr"], "packages.x86_64-linux.broken");
        assert_eq!(v["error"], "boom");
        assert!(v.get("drvPath").is_none(), "no drvPath on failure");
    }
}
