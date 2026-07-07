/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::attr_spec;
use clap::Args;
use std::io::Write;
use std::path::Path;

#[derive(Args, Debug)]
pub struct EvalArgs {
    /// Attribute wildcard patterns, e.g. 'checks.*.*' 'packages.x86_64-linux.*'.
    /// Accepts installable syntax ('.#gradient-cli-full' or
    /// 'github:NixOS/patchelf#hydraJobs.*'); a bare attr is qualified as
    /// 'packages.<system>.<attr>' like 'nix eval', the flake part sets the flake
    /// to evaluate and defaults to the current directory.
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
    let system = attr_spec::default_nix_system();
    let (flake_ref, wildcards) = split_installables(&args.patterns, &system);
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

/// Pull the flake reference out of installable-form patterns ('<ref>#<attr>')
/// and qualify each attr like `nix eval`, so `gradient eval .#gradient-cli-full`
/// resolves `packages.<system>.gradient-cli-full` on the current flake. A
/// non-empty flake part sets the flake (default '.'); known output categories
/// and wildcards pass through untouched (see [`attr_spec::qualify_attr`]).
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

/// Resolve a local flake reference for the Nix C API, which - unlike the CLI -
/// needs an absolute path and does not read the working directory. A directory
/// inside a git checkout becomes a `git+file://` flake (with `?dir=` for a
/// sub-flake), exactly like `nix eval .`, so only tracked files are evaluated. A
/// bare `path:` flake would instead copy the WHOLE directory into the store
/// first - gitignored build artefacts and all (`target/`, `node_modules`,
/// `result`) - which is orders of magnitude slower to evaluate. A directory
/// outside any git repo falls back to its absolute `path:` form; a scheme ref
/// (github:, path:, git+…) or registry name has no on-disk target, so
/// canonicalisation fails and it passes through unchanged.
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

/// The `git+file://` flake URL `nix` uses for a local checkout: the work-tree
/// root as a `file://` path, plus `?dir=` when the flake lives in a subdirectory
/// of the repo.
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
        let (flake, wildcards) =
            split_installables(&[".#packages.x86_64-linux.hello".to_string()], "x86_64-linux");
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
        assert_eq!(resolve_flake_ref("github:NixOS/nixpkgs"), "github:NixOS/nixpkgs");
        assert_eq!(resolve_flake_ref("path:/abs"), "path:/abs");
        // A non-existent local path can't be canonicalised, so it is left as-is.
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
            git_flake_url(Path::new("/home/u/repo"), Path::new("/home/u/repo/sub/flake")),
            "git+file:///home/u/repo?dir=sub/flake"
        );
    }
}
