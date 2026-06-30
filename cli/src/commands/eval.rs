/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use std::io::Write;

#[derive(Args, Debug)]
pub struct EvalArgs {
    /// Attribute wildcard patterns, e.g. 'checks.*.*' 'packages.x86_64-linux.*'
    #[arg(required = true, value_name = "PATTERN")]
    patterns: Vec<String>,
    /// Flake reference to evaluate
    #[arg(long, default_value = ".", value_name = "REF")]
    flake: String,
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
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let result = gradient_eval::jobs::eval_jobs(&args.flake, &args.patterns, |job| {
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
