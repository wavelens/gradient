/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Nix flake evaluator backed by the high-level `nix-bindings` wrappers.
//!
//! Holds one `EvalState` (built with flake support + a warm on-disk eval cache)
//! and hands out a [`FlakeWalker`](crate::flake_walk::FlakeWalker) per
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
    flake_settings: Arc<FlakeSettings>,
    fetch_settings: FetchersSettings,
    state: EvalState,
}

// SAFETY: NixEvaluator is only used from one thread at a time (spawn_blocking).
unsafe impl Send for NixEvaluator {}
unsafe impl Sync for NixEvaluator {}

/// New-master nix refuses to remount a read-only `/nix/store` writable unless it
/// created its own private mount namespace (its CLI does this in `main`); the
/// eval-worker drives libnixstore directly, so mirror it here. As root on a
/// read-only store, unshare a mount namespace and remount the store writable.
/// No-op off Linux, when not root, or when the store is already writable (prod).
#[cfg(target_os = "linux")]
fn ensure_store_writable() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let store = c"/nix/store".as_ptr();
        if libc::geteuid() != 0 {
            return;
        }
        let mut vfs: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(store, &mut vfs) != 0 || (vfs.f_flag & libc::ST_RDONLY) == 0 {
            return;
        }
        if libc::unshare(libc::CLONE_NEWNS) != 0 {
            return;
        }
        libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        );
        let flags = [
            (libc::ST_NODEV, libc::MS_NODEV),
            (libc::ST_NOSUID, libc::MS_NOSUID),
            (libc::ST_NOEXEC, libc::MS_NOEXEC),
            (libc::ST_NOATIME, libc::MS_NOATIME),
            (libc::ST_NODIRATIME, libc::MS_NODIRATIME),
            (libc::ST_RELATIME, libc::MS_RELATIME),
        ]
        .into_iter()
        .fold(libc::MS_REMOUNT | libc::MS_BIND, |acc, (st, ms)| {
            if (vfs.f_flag & st) != 0 {
                acc | ms
            } else {
                acc
            }
        });
        libc::mount(
            std::ptr::null(),
            store,
            std::ptr::null(),
            flags,
            std::ptr::null(),
        );
    });
}

#[cfg(not(target_os = "linux"))]
fn ensure_store_writable() {}

impl NixEvaluator {
    // nix-bindings' Context/Store/FlakeSettings aren't Send+Sync, but the C API
    // mandates Arc (LockFlags holds an Arc<FlakeSettings>); NixEvaluator is only
    // ever touched from one thread (Boehm GC + spawn_blocking).
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn new() -> Result<Self> {
        ensure_store_writable();
        let ctx = Arc::new(Context::new().context("nix context init")?);
        ctx.set_setting("show-trace", "true")?;
        ctx.set_setting("builders", "")?;
        ctx.set_setting("build-hook", "")?;

        let store = Arc::new(Store::open(&ctx, None).context("nix store open")?);
        let flake_settings = Arc::new(FlakeSettings::new(&ctx)?);
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

    /// Cheap cumulative Nix evaluator counters (thunks, calls, GC gauges).
    /// The caller diffs successive reads to get a per-request delta.
    pub fn stats(&self) -> Result<nix_bindings::EvalStats> {
        self.state
            .stats()
            .map_err(|e| anyhow::anyhow!("eval stats: {e}"))
    }

    /// Lock `flake_ref` (with `overrides` applied at lock time) and open its
    /// eval cache, returning a walker that reuses the one locked flake + warm
    /// cursor for all discover/resolve calls.
    pub fn walker(
        &self,
        flake_ref: &str,
        overrides: &[(String, String)],
    ) -> Result<crate::flake_walk::FlakeWalker<'_>> {
        crate::flake_walk::FlakeWalker::open(
            &self.ctx,
            &self.fetch_settings,
            &self.flake_settings,
            &self.state,
            flake_ref,
            overrides,
        )
    }

    /// Lock `flake_ref` (with `overrides` applied) and return its eval-cache
    /// fingerprint without evaluating or creating the on-disk eval cache.
    /// `None` for mutable flakes.
    pub fn fingerprint(
        &self,
        flake_ref: &str,
        overrides: &[(String, String)],
    ) -> Result<Option<String>> {
        crate::flake_walk::fingerprint(
            &self.ctx,
            &self.fetch_settings,
            &self.flake_settings,
            &self.state,
            &self.store,
            flake_ref,
            overrides,
        )
    }
}
