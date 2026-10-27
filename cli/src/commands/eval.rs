/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::attr_spec;
use clap::Args;
use etcetera::base_strategy::{BaseStrategy, choose_base_strategy};
use gradient_eval::ipc::{License, ResolvedItem};
use gradient_eval::pool::{
    EvalPool, EvalPoolConfig, MemoryGuard, RefuseDependencies, default_workers,
};
use serde::Serialize;
use std::io::Write;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::Path;
use std::sync::PoisonError;

#[derive(Args, Debug)]
pub struct EvalArgs {
    /// Attribute wildcard patterns, e.g. 'checks.*.*' 'packages.x86_64-linux.*'. Patterns also
    /// accept installable syntax ('.#gradient-cli-full' or 'github:NixOS/patchelf#hydraJobs.*').
    /// Attrs without a prefix qualify as 'packages.<system>.<attr>' like 'nix eval'. The parts
    /// before the '#' name the flake to evaluate, defaulting to the current directory.
    #[arg(required = true, value_name = "PATTERN")]
    patterns: Vec<String>,

    /// Evaluation subprocesses in the pool, defaulting to the host's core count, capped at 16.
    #[arg(long, value_name = "N")]
    workers: Option<NonZeroUsize>,

    /// Resident memory in MiB for an idle evaluation subprocess, restarted between calls to free
    /// the Nix heap.
    #[arg(long, value_name = "MiB", default_value_t = NonZeroU64::new(2048).expect("nonzero"))]
    max_rss: NonZeroU64,

    /// Ceiling in MiB for the resident memory of all evaluation subprocesses together.
    #[arg(long, value_name = "MiB")]
    max_memory: Option<NonZeroU64>,
}

#[derive(Debug, Serialize)]
pub struct Job {
    pub attr: String,
    #[serde(rename = "attrPath")]
    pub attr_path: Vec<String>,
    #[serde(rename = "drvPath", skip_serializing_if = "Option::is_none")]
    pub drv_path: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub licenses: Vec<License>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Job {
    fn from_item(item: ResolvedItem) -> Self {
        Job {
            attr_path: gradient_eval::wildcard_walk::parse_pattern(&item.attr).1,
            attr: item.attr,
            drv_path: item.drv_path.map(|drv| gradient_eval::nix_store_path(&drv)),
            licenses: item.licenses,
            error: item.error,
        }
    }

    fn failed(attr: String, error: String) -> Self {
        Job {
            attr_path: gradient_eval::wildcard_walk::parse_pattern(&attr).1,
            attr,
            drv_path: None,
            licenses: vec![],
            error: Some(error),
        }
    }
}

pub fn subprocess_flags(args: &[String]) -> Option<bool> {
    let has = |flag: &str| args.iter().any(|arg| arg == flag);
    has("--eval-subprocess").then(|| has("--eval-metrics"))
}

pub fn run(args: EvalArgs) -> std::io::Result<()> {
    let system = attr_spec::default_nix_system();
    let (flake_ref, wildcards) = split_installables(&args.patterns, &system);
    let flake_ref = resolve_flake_ref(&flake_ref);

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(evaluate(flake_ref, wildcards, args))
}

async fn evaluate(
    flake_ref: String,
    wildcards: Vec<String>,
    args: EvalArgs,
) -> std::io::Result<()> {
    let pool = EvalPool::new(EvalPoolConfig {
        workers: args.workers.unwrap_or_else(default_workers).get(),
        max_rss: args.max_rss.get() * 1024 * 1024,
        memory: guard_of(args.max_memory),
        cache_dir: eval_cache_dir(),
        trace_dir: None,
        metrics: false,
        fetch_through_parent: false,
    });

    let out = std::sync::Mutex::new(std::io::stdout());
    let print = |job: Job| {
        if let Ok(line) = serde_json::to_string(&job) {
            let mut out = out.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = writeln!(out, "{line}");
        }
    };

    let dependencies = RefuseDependencies("the local evaluation");
    let found = |items: Vec<ResolvedItem>| items.into_iter().map(Job::from_item).for_each(print);

    let evaluated = if all_concrete(&wildcards) {
        pool.list_attrs(flake_ref, wildcards, &[], &dependencies, &found)
            .await
    } else {
        pool.list_shards(flake_ref, wildcards, &[], &dependencies, &found)
            .await
    };

    let outcome = match evaluated {
        Ok(discovery) => {
            for error in discovery.errors {
                print(Job::failed(error.attr, error.message));
            }
            Ok(())
        }
        Err(e) => Err(e),
    };
    pool.shutdown().await;

    let mut out = out.into_inner().unwrap_or_else(PoisonError::into_inner);
    out.flush()?;
    match outcome {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("gradient eval: {e:#}");
            std::process::exit(1);
        }
    }
}

/// Concrete sets skip the discovery walk: siblings cost orders of magnitude more.
fn all_concrete(wildcards: &[String]) -> bool {
    !wildcards.is_empty()
        && wildcards
            .iter()
            .all(|w| !w.starts_with('!') && w.split('.').all(|seg| !seg.contains('*')))
}

/// The CLI has no free-RAM guard, so a memory flag is a ceiling on the whole pool or nothing.
fn guard_of(max_memory: Option<NonZeroU64>) -> MemoryGuard {
    max_memory.map_or(MemoryGuard::None, |mib| {
        MemoryGuard::MaxTotal(mib.get() * 1024 * 1024)
    })
}

/// The user's eval cache, apart from `nix eval`'s `~/.cache/nix`.
fn eval_cache_dir() -> String {
    let base = choose_base_strategy()
        .expect("Could not find a cache directory")
        .cache_dir();
    base.join("gradient")
        .join("eval-cache")
        .to_string_lossy()
        .into_owned()
}

fn split_installables(patterns: &[String], system: &str) -> (String, Vec<String>) {
    let mut flake_ref = ".".to_string();
    let mut wildcards = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        let (excl, body) = pattern
            .strip_prefix('!')
            .map(|r| ("!", r))
            .unwrap_or(("", pattern.as_str()));
        let attr = match body.split_once('#') {
            Some((reference, attr)) => {
                if !reference.is_empty() {
                    flake_ref = reference.to_string();
                }
                attr
            }
            None => body,
        };
        wildcards.push(attr_spec::qualify_attr(&format!("{excl}{attr}"), system));
    }
    (flake_ref, wildcards)
}

/// The Nix C API needs an absolute path and is not reading the working directory. A directory
/// inside a git checkout is becoming a `git+file://` flake, exactly like `nix eval .`. A bare
/// `path:` flake would copy the whole directory into the store first, gitignored artefacts
/// included. A scheme ref or registry name is failing canonicalisation and passing through
/// unchanged.
fn resolve_flake_ref(flake: &str) -> String {
    let Ok(abs) = std::fs::canonicalize(flake) else {
        return flake.to_string();
    };

    match git2::Repository::discover(&abs)
        .ok()
        .and_then(|repo| repo.workdir().map(Path::to_path_buf))
    {
        Some(root) => git_flake_url(&root, &abs),
        None => abs.to_string_lossy().into_owned(),
    }
}

fn git_flake_url(root: &Path, target: &Path) -> String {
    let root_str = root.to_string_lossy();
    let mut url = format!("git+file://{}", root_str.trim_end_matches('/'));
    if let Ok(rel) = target.strip_prefix(root)
        && !rel.as_os_str().is_empty()
    {
        url.push_str("?dir=");
        url.push_str(&rel.to_string_lossy());
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_installable_flake_and_attr() {
        let (flake, wildcards) = split_installables(
            &[".#packages.x86_64-linux.hello".to_string()],
            "x86_64-linux",
        );
        assert_eq!(flake, ".");
        assert_eq!(wildcards, vec!["packages.x86_64-linux.hello".to_string()]);
    }

    #[test]
    fn bare_installable_qualifies_to_packages() {
        let (flake, wildcards) =
            split_installables(&[".#gradient-cli-full".to_string()], "x86_64-linux");
        assert_eq!(flake, ".");
        assert_eq!(
            wildcards,
            vec!["packages.x86_64-linux.gradient-cli-full".to_string()]
        );
    }

    #[test]
    fn installable_flake_overrides_default_and_qualifies() {
        let (flake, wildcards) =
            split_installables(&["github:NixOS/nixpkgs#hello".to_string()], "x86_64-linux");
        assert_eq!(flake, "github:NixOS/nixpkgs");
        assert_eq!(wildcards, vec!["packages.x86_64-linux.hello".to_string()]);
    }

    #[test]
    fn bare_patterns_default_to_current_dir() {
        let (flake, wildcards) = split_installables(
            &[
                "packages.x86_64-linux.*".to_string(),
                "checks.*.*".to_string(),
            ],
            "x86_64-linux",
        );
        assert_eq!(flake, ".");
        assert_eq!(wildcards, vec!["packages.x86_64-linux.*", "checks.*.*"]);
    }

    #[test]
    fn scheme_refs_and_missing_paths_pass_through_unresolved() {
        assert_eq!(
            resolve_flake_ref("github:NixOS/nixpkgs"),
            "github:NixOS/nixpkgs"
        );
        assert_eq!(resolve_flake_ref("path:/abs"), "path:/abs");
        assert_eq!(resolve_flake_ref("./no-such-dir-xyz"), "./no-such-dir-xyz");
    }

    #[test]
    fn git_checkout_root_is_a_git_file_flake() {
        assert_eq!(
            git_flake_url(Path::new("/home/u/repo/"), Path::new("/home/u/repo")),
            "git+file:///home/u/repo"
        );
    }

    #[test]
    fn git_subdir_flake_carries_a_dir_query() {
        assert_eq!(
            git_flake_url(
                Path::new("/home/u/repo"),
                Path::new("/home/u/repo/sub/flake")
            ),
            "git+file:///home/u/repo?dir=sub/flake"
        );
    }

    #[test]
    fn a_resolved_item_becomes_a_job_line() {
        let line = serde_json::to_string(&Job::from_item(ResolvedItem {
            attr: "packages.x86_64-linux.hello".into(),
            drv_path: Some("aaaa-hello.drv".into()),
            licenses: vec![License::Spdx {
                id: "GPL-3.0-or-later".into(),
            }],
            error: None,
            cost: None,
            exec: None,
        }))
        .expect("serialize");

        assert_eq!(
            line,
            r#"{"attr":"packages.x86_64-linux.hello","attrPath":["packages","x86_64-linux","hello"],"drvPath":"/nix/store/aaaa-hello.drv","licenses":[{"id":"GPL-3.0-or-later"}]}"#
        );
    }

    #[test]
    fn a_failed_item_carries_only_its_error() {
        let line = serde_json::to_string(&Job::from_item(ResolvedItem {
            attr: "packages.x86_64-linux.broken".into(),
            drv_path: None,
            licenses: vec![],
            error: Some("boom".into()),
            cost: None,
            exec: None,
        }))
        .expect("serialize");

        assert_eq!(
            line,
            r#"{"attr":"packages.x86_64-linux.broken","attrPath":["packages","x86_64-linux","broken"],"error":"boom"}"#
        );
    }

    #[test]
    fn an_all_concrete_set_skips_the_discovery_walk() {
        assert!(all_concrete(&["packages.x86_64-linux.hello".to_string()]));
        assert!(!all_concrete(&["packages.x86_64-linux.*".to_string()]));
        assert!(!all_concrete(&["!packages.x86_64-linux.hello".to_string()]));
        assert!(!all_concrete(&[]));
    }

    #[test]
    fn the_subprocess_flags_are_read_before_clap() {
        let flag = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
            subprocess_flags(&args)
        };
        assert_eq!(flag(&["gradient"]), None);
        assert_eq!(flag(&["gradient", "--eval-subprocess"]), Some(false));
        assert_eq!(
            flag(&["gradient", "--eval-subprocess", "--eval-metrics"]),
            Some(true)
        );
    }

    #[derive(clap::Parser)]
    struct EvalCli {
        #[command(flatten)]
        eval: EvalArgs,
    }

    fn parse(args: &[&str]) -> EvalArgs {
        <EvalCli as clap::Parser>::parse_from(args).eval
    }

    #[test]
    fn the_flags_default_to_the_pool_sizing() {
        let args = parse(&["gradient", "packages.x86_64-linux.*"]);
        assert!(
            args.workers.is_none(),
            "an unset worker count is the host's core count, capped at 16"
        );
        assert_eq!(args.max_rss.get(), 2048);
        assert_eq!(args.max_memory, None);
    }

    #[test]
    fn the_flags_are_read_as_mib() {
        let args = parse(&[
            "gradient",
            "--workers",
            "4",
            "--max-rss",
            "512",
            "--max-memory",
            "1024",
            "packages.x86_64-linux.*",
        ]);
        assert_eq!(args.workers.map(std::num::NonZeroUsize::get), Some(4));
        assert_eq!(args.max_rss.get(), 512);
        assert_eq!(args.max_memory.map(std::num::NonZeroU64::get), Some(1024));
    }

    #[test]
    fn only_a_memory_flag_arms_a_guard() {
        assert_eq!(guard_of(None), MemoryGuard::None);
        assert_eq!(
            guard_of(std::num::NonZeroU64::new(1024)),
            MemoryGuard::MaxTotal(1024 * 1024 * 1024)
        );
    }
}
