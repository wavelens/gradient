/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use std::io::Write;

#[derive(Args, Debug)]
pub struct EvalArgs {
    /// Attribute wildcard patterns, e.g. 'checks.*.*' 'packages.x86_64-linux.*'.
    /// Accepts installable syntax ('.#packages.x86_64-linux.hello' or
    /// 'github:NixOS/patchelf#hydraJobs.*'); the flake part sets the flake to
    /// evaluate and defaults to the current directory.
    #[arg(required = true, value_name = "PATTERN")]
    patterns: Vec<String>,
}

/// Evaluate a flake's outputs to derivations, like nix-eval-jobs, using the
/// gradient worker evaluator. Streams one JSON line per attribute to stdout.
///
/// Runs synchronously without a Tokio runtime: the Nix C API uses Boehm GC,
/// which must run isolated from Tokio's thread pool (see the worker's eval
/// subprocess). Per-attribute failures are reported in their JSON line and do
/// not abort the run; only a top-level failure (e.g. locking the flake) exits
/// non-zero.
pub fn run(args: EvalArgs) -> std::io::Result<()> {
    let (flake_ref, wildcards) = split_installables(&args.patterns);
    let flake_ref = resolve_flake_ref(&flake_ref);

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let result = gradient_eval::jobs::eval_jobs(&flake_ref, &wildcards, |job| {
        if let Ok(line) = serde_json::to_string(&job) {
            let _ = writeln!(out, "{line}");
        }
    });

    out.flush()?;
    if let Err(e) = result {
        eprintln!("gradient eval: {e:#}");
        std::process::exit(1);
    }
    Ok(())
}

/// Pull the flake reference out of installable-form patterns ('<ref>#<attr>'),
/// so `gradient eval .#packages.x86_64-linux.hello` behaves like `gradient
/// build`. A non-empty flake part sets the flake (default '.'); bare patterns
/// are kept as-is.
fn split_installables(patterns: &[String]) -> (String, Vec<String>) {
    let mut flake_ref = ".".to_string();
    let mut wildcards = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        match pattern.split_once('#') {
            Some((reference, attr)) => {
                if !reference.is_empty() {
                    flake_ref = reference.to_string();
                }
                wildcards.push(attr.to_string());
            }
            None => wildcards.push(pattern.clone()),
        }
    }
    (flake_ref, wildcards)
}

/// The Nix C API's flake-reference parser demands an absolute path and, unlike
/// the CLI, does not resolve one relative to the working directory. Convert a
/// local path ('.', './sub', relative dirs) to an absolute one; a scheme ref
/// (github:, path:, git+…) or a registry name has no on-disk target, so
/// canonicalisation fails and it passes through unchanged.
fn resolve_flake_ref(flake: &str) -> String {
    match std::fs::canonicalize(flake) {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(_) => flake.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_installable_flake_and_attr() {
        let (flake, wildcards) =
            split_installables(&[".#packages.x86_64-linux.hello".to_string()]);
        assert_eq!(flake, ".");
        assert_eq!(wildcards, vec!["packages.x86_64-linux.hello".to_string()]);
    }

    #[test]
    fn installable_flake_overrides_default() {
        let (flake, wildcards) = split_installables(&["github:NixOS/nixpkgs#hello".to_string()]);
        assert_eq!(flake, "github:NixOS/nixpkgs");
        assert_eq!(wildcards, vec!["hello".to_string()]);
    }

    #[test]
    fn bare_patterns_default_to_current_dir() {
        let (flake, wildcards) = split_installables(&[
            "packages.x86_64-linux.*".to_string(),
            "checks.*.*".to_string(),
        ]);
        assert_eq!(flake, ".");
        assert_eq!(wildcards, vec!["packages.x86_64-linux.*", "checks.*.*"]);
    }

    #[test]
    fn scheme_refs_pass_through_unresolved() {
        assert_eq!(resolve_flake_ref("github:NixOS/nixpkgs"), "github:NixOS/nixpkgs");
        assert_eq!(resolve_flake_ref("path:/abs"), "path:/abs");
    }

    #[test]
    fn local_path_is_made_absolute() {
        assert_eq!(resolve_flake_ref("."), std::env::current_dir().unwrap().to_string_lossy());
    }
}
