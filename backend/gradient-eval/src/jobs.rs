/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! nix-eval-jobs-style streaming driver over the gradient evaluator: discover
//! the attribute paths matching a set of wildcards, resolve each to its
//! `.drv`, and report one [`Job`] per attribute. Per-attribute failures are
//! reported in the `Job` (mirroring nix-eval-jobs) instead of aborting.

use anyhow::Result;
use serde::Serialize;

use crate::nix_eval::NixEvaluator;
use crate::nix_store_path;

/// One newline-delimited JSON record, shaped after nix-eval-jobs' output.
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
    /// A successfully resolved attribute. `drv` is a bare hash-name; the full
    /// `/nix/store` path is emitted to match nix-eval-jobs.
    pub fn resolved(attr: String, drv: String, references: Vec<String>) -> Self {
        Job {
            attr_path: attr.split('.').map(str::to_string).collect(),
            attr,
            drv_path: Some(nix_store_path(&drv)),
            references,
            error: None,
        }
    }

    /// An attribute whose evaluation failed.
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

/// Evaluate `wildcards` against `flake_ref`, invoking `sink` once per resolved
/// attribute as soon as it is resolved.
///
/// Concrete attr paths (no `*`/`#`/`!`) are resolved directly, skipping the
/// output-tree discovery walk: `gradient eval .#gradient-cli-full` then forces
/// exactly that attribute, like `nix eval .#gradient-cli-full`, instead of
/// walking siblings (which, for a flake whose `checks` are NixOS VM tests, costs
/// orders of magnitude more). A set that contains any `*`/`#` wildcard or `!`
/// exclusion falls back to the discovery walk over all patterns, since an
/// exclusion is applied across the whole include set.
///
/// Synchronous and Boehm-GC bound: call from a context without a Tokio runtime
/// (the CLI runs it before the runtime starts, mirroring the eval worker).
pub fn eval_jobs(flake_ref: &str, wildcards: &[String], mut sink: impl FnMut(Job)) -> Result<()> {
    let evaluator = NixEvaluator::new()?;
    let walker = evaluator.walker(flake_ref)?;

    let attrs = if wildcards.iter().all(|w| is_concrete_attr(w)) {
        wildcards.to_vec()
    } else {
        walker.discover(wildcards)?.0
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

/// A wildcard-free include: no `*`/`#` segment and no `!` exclusion. Such a
/// pattern names exactly one attribute, so
/// [`FlakeWalker::resolve`](crate::flake_walk::FlakeWalker::resolve) reaches it
/// directly and the discovery walk can be skipped. An exclusion only prunes
/// wildcard matches, so its presence keeps the whole set on the discovery path.
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
