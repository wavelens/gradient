/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Nix flake evaluator backed by the high-level `nix-bindings` wrappers.
//!
//! Holds one `EvalState` (built with flake support + a warm on-disk eval cache)
//! and hands out a [`FlakeWalker`](crate::nix::flake_walk::FlakeWalker) per
//! flake reference that drives a cursor walk over its output attribute tree.
//!
//! `nix_bindings` embeds Boehm GC into the process. Boehm GC cannot coexist
//! with Tokio's thread pool: it requires stop-the-world signal delivery to all
//! threads, but Tokio worker threads block those signals. Every method on
//! `NixEvaluator` is therefore **synchronous** and must be invoked from a
//! blocking context (e.g. `tokio::task::spawn_blocking`).

use std::sync::Arc;

use anyhow::{Context as _, Result};
use nix_bindings::flake::{FetchersSettings, FlakeSettings};
use nix_bindings::{Context, EvalState, EvalStateBuilder, Store};

/// Evaluates flake outputs through the embedded Nix C API.
///
/// Create one instance per evaluation session. All methods are **synchronous**
/// and must be called from a blocking context (e.g. `tokio::task::spawn_blocking`).
pub struct NixEvaluator {
    ctx: Arc<Context>,
    store: Arc<Store>,
    flake_settings: FlakeSettings,
    fetch_settings: FetchersSettings,
    state: EvalState,
}

// SAFETY: NixEvaluator is only used from one thread at a time (spawn_blocking).
unsafe impl Send for NixEvaluator {}
unsafe impl Sync for NixEvaluator {}

impl NixEvaluator {
    pub fn new() -> Result<Self> {
        let ctx = Arc::new(Context::new().context("nix context init")?);
        ctx.set_setting("show-trace", "true")?;
        ctx.set_setting("builders", "")?;
        ctx.set_setting("build-hook", "")?;

        let store = Arc::new(Store::open(&ctx, None).context("nix store open")?);
        let flake_settings = FlakeSettings::new(&ctx)?;
        let fetch_settings = FetchersSettings::new(&ctx)?;

        // eval-cache + pure-eval are EvalState-scoped and required for a warm
        // on-disk eval cache, so they are set on the builder, not globally.
        let state = EvalStateBuilder::new(&store)?
            .with_flake_settings(&flake_settings)?
            .set_setting("eval-cache", "true")?
            .set_setting("pure-eval", "true")?
            .build()
            .context("nix eval state build")?;

        Ok(NixEvaluator {
            ctx,
            store,
            flake_settings,
            fetch_settings,
            state,
        })
    }

    /// Lock `flake_ref` and open its eval cache, returning a walker that reuses
    /// the one locked flake + warm cursor for all discover/resolve calls.
    pub(crate) fn walker(&self, flake_ref: &str) -> Result<crate::nix::flake_walk::FlakeWalker<'_>> {
        crate::nix::flake_walk::FlakeWalker::open(
            &self.ctx,
            &self.fetch_settings,
            &self.flake_settings,
            &self.state,
            flake_ref,
        )
    }

    /// Lock `flake_ref` and return its eval-cache fingerprint without
    /// evaluating or creating the on-disk eval cache. `None` for mutable flakes.
    pub(crate) fn fingerprint(&self, flake_ref: &str) -> Result<Option<String>> {
        crate::nix::flake_walk::fingerprint(
            &self.ctx,
            &self.fetch_settings,
            &self.flake_settings,
            &self.state,
            &self.store,
            flake_ref,
        )
    }
}
