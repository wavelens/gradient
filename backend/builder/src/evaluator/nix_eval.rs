/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Nix expression evaluator backed by the Nix C API via `nix_bindings`.
//!
//! `nix_bindings` embeds Boehm GC into the process. Boehm GC cannot coexist
//! with Tokio's thread pool: it requires stop-the-world signal delivery to all
//! threads, but Tokio worker threads block those signals. Every method on
//! `NixEvaluator` is therefore **synchronous** and must be invoked from a
//! blocking context (e.g. `tokio::task::spawn_blocking`).

use anyhow::{Context as _, Result};
use nix_bindings::{Context, EvalState, EvalStateBuilder, Store};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// NixEvaluator
// ---------------------------------------------------------------------------

/// Evaluates Nix expressions through the embedded Nix C API.
///
/// Create one instance per evaluation session. All methods are **synchronous**
/// and must be called from a blocking context (e.g. `tokio::task::spawn_blocking`).
pub struct NixEvaluator {
    state: EvalState,
    // Keep store and context alive for the lifetime of the evaluator.
    _store: Arc<Store>,
    _ctx: Arc<Context>,
}

impl NixEvaluator {
    pub fn new() -> Result<Self> {
        let ctx = Arc::new(Context::new().context("failed to create nix context")?);
        let store = Arc::new(Store::open(&ctx, None).context("failed to open nix store")?);
        let state = EvalStateBuilder::new(&store)
            .context("failed to create eval state builder")?
            .build()
            .context("failed to build eval state")?;

        Ok(NixEvaluator {
            state,
            _store: store,
            _ctx: ctx,
        })
    }

    /// Evaluate `expr` as an attrset and return its attribute names.
    /// Returns `Err` if `expr` does not evaluate to an attrset or nix fails.
    pub fn attr_names(&self, expr: &str) -> Result<Vec<String>> {
        // The high-level API does not expose attrset iteration, so route the
        // attribute names through `builtins.toJSON` and parse the JSON string.
        let wrapped = format!("builtins.toJSON (builtins.attrNames ({}))", expr);
        let mut value = self
            .state
            .eval_from_string(&wrapped, "<gradient>")
            .context("nix eval failed")?;
        value.force().context("failed to force nix value")?;
        let json = value
            .as_string()
            .context("nix eval did not return a string")?;
        serde_json::from_str(&json).context("failed to parse attrNames JSON")
    }

    /// Evaluate `expr` and return it as a string.
    /// Returns `Err` if `expr` does not evaluate to a string or nix fails.
    pub fn eval_string(&self, expr: &str) -> Result<String> {
        let mut value = self
            .state
            .eval_from_string(expr, "<gradient>")
            .context("nix eval failed")?;
        value.force().context("failed to force nix value")?;
        value
            .as_string()
            .context("nix value is not a string")
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Escape a string for embedding inside a Nix double-quoted string literal.
pub fn escape_nix_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
