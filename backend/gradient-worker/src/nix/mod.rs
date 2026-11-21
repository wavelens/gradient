/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The flake evaluator lives in the shared `gradient-eval` crate for reuse by the CLI.
//! It is re-exported here to keep existing `crate::nix::...` paths resolving.

pub use gradient_eval::{eval_worker, wildcard_walk};

pub mod gcroots;
pub mod log;
pub mod store;
pub mod system_features;

pub fn check_flake_lock_with_allowlist(
    lock_file: &serde_json::Value,
    flake_dir: Option<&str>,
    override_references: &[&str],
) -> Result<(), String> {
    gradient_types::fetch_source_allowlist_and_request_validation::validate_flake_lock_computed_during_evaluation(
        lock_file,
        flake_dir,
        override_references,
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use gradient_eval::nix_eval::NixEvaluator;

    fn nix_executable_exists_in_path() -> bool {
        #[expect(
            clippy::disallowed_methods,
            reason = "PATH is an operating system variable."
        )]
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path).any(|dir| dir.join("nix").is_file())
    }

    #[test]
    fn flake_nix_only_path_input_missing_from_flake_lock_is_refused_by_allowlist_check_before_output_evaluation()
     {
        if !nix_executable_exists_in_path() {
            eprintln!("skipped: no nix binary in PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let flake = dir.path().join("flake");
        std::fs::create_dir_all(&flake).unwrap();
        let host_file = dir.path().join("host-file");
        std::fs::write(&host_file, "host content").unwrap();
        let nar_hash = std::process::Command::new("nix")
            .args([
                "--extra-experimental-features",
                "nix-command",
                "hash",
                "path",
            ])
            .arg(&host_file)
            .output()
            .unwrap();
        let nar_hash = String::from_utf8(nar_hash.stdout).unwrap();
        std::fs::write(
            flake.join("flake.nix"),
            format!(
                r#"{{ inputs.s = {{ url = "path:{}?narHash={}"; flake = false; }}; outputs = {{ s, ... }}: throw "Nix evaluated the outputs."; }}"#,
                host_file.display(),
                nar_hash.trim().replace('+', "%2B").replace('=', "%3D")
            ),
        )
        .unwrap();
        std::fs::write(
            flake.join("flake.lock"),
            r#"{"nodes":{"root":{}},"root":"root","version":7}"#,
        )
        .unwrap();
        let evaluator =
            NixEvaluator::new(None, None, Some(super::check_flake_lock_with_allowlist)).unwrap();

        let err = evaluator
            .walker(&format!("path:{}", flake.display()), &[])
            .err()
            .expect("Allowlist check rejection of local input");

        let err = format!("{err:#}");
        assert!(
            err.contains("disallowed input: Rejected flake.lock input `s`"),
            "{err}"
        );
        assert!(!err.contains("Nix evaluated the outputs."), "{err}");
    }
}
