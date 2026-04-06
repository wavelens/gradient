/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Nix expression evaluator backed by `nix eval --json` subprocesses.
//!
//! Using the Nix C API directly embeds Boehm GC into the server process.
//! Boehm GC cannot coexist with Tokio's thread pool: it requires stop-the-world
//! signal delivery to all threads, but Tokio threads block those signals, causing
//! "Collecting from unknown thread" → `abort()` crashes.
//!
//! Running `nix eval` as a child process completely isolates Boehm GC: each
//! child is single-threaded from GC's perspective and is cleaned up by the OS
//! when the evaluation finishes.

use anyhow::{Context, Result};
use std::process::Command;

// ---------------------------------------------------------------------------
// NixEvaluator
// ---------------------------------------------------------------------------

/// Evaluates Nix expressions by spawning `nix eval --json --expr` subprocesses.
///
/// Create one instance per evaluation session.  All methods are **synchronous**
/// and must be called from a blocking context (e.g. `tokio::task::spawn_blocking`).
pub struct NixEvaluator;

impl NixEvaluator {
    pub fn new() -> Result<Self> {
        Ok(NixEvaluator)
    }

    /// Evaluate `expr` as an attrset and return its attribute names.
    /// Returns `Err` if `expr` does not evaluate to an attrset or nix fails.
    pub fn attr_names(&self, expr: &str) -> Result<Vec<String>> {
        let full_expr = format!("builtins.attrNames ({})", expr);
        let output = Command::new("nix")
            .args(["eval", "--json", "--expr", &full_expr])
            .output()
            .context("failed to spawn nix")?;

        if !output.status.success() {
            anyhow::bail!(
                "nix eval failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        serde_json::from_slice(&output.stdout).context("failed to parse nix eval output as JSON array")
    }

    /// Evaluate `expr` and return it as a string.
    /// Returns `Err` if `expr` does not evaluate to a string or nix fails.
    pub fn eval_string(&self, expr: &str) -> Result<String> {
        let output = Command::new("nix")
            .args(["eval", "--json", "--expr", expr])
            .output()
            .context("failed to spawn nix")?;

        if !output.status.success() {
            anyhow::bail!(
                "nix eval failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        serde_json::from_slice(&output.stdout).context("failed to parse nix eval output as JSON string")
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Escape a string for embedding inside a Nix double-quoted string literal.
pub fn escape_nix_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
