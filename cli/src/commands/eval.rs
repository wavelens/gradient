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
    /// Attribute wildcard patterns, e.g. 'checks.*.*' 'packages.x86_64-linux.*'. Installable syntax
    /// ('.#gradient-cli-full' or 'github:NixOS/patchelf#hydraJobs.*') is accepted. A bare attr is
    /// qualified as 'packages.<system>.<attr>' like 'nix eval'. The flake part is selecting the
    /// flake to evaluate, with the current directory as default.
    #[arg(required = true, value_name = "PATTERN")]
    patterns: Vec<String>,
}

/// This is running without a Tokio runtime. The Nix C API is using Boehm GC, which must stay
/// isolated from Tokio's thread pool.
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
}
