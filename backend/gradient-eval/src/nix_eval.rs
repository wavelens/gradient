/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `nix_bindings` is embedding Boehm GC, which is requiring stop-the-world signals on every thread.
//! Tokio worker threads are blocking those signals. Every `NixEvaluator` method must run in a
//! blocking context such as `spawn_blocking`.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use nix_bindings::flake::{FetchersSettings, FlakeSettings};
use nix_bindings::{Context, EvalState, EvalStateBuilder, Store};

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

/// New-master nix is refusing to remount a read-only `/nix/store` writable outside its own mount
/// namespace. The nix CLI is creating that namespace in `main`, and the eval worker must mirror it.
/// This is a no-op off Linux, when not root, or when the store is already writable.
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

    pub fn stats(&self) -> Result<nix_bindings::EvalStats> {
        self.state
            .stats()
            .map_err(|e| anyhow::anyhow!("eval stats: {e}"))
    }

    pub fn stats_with(&self, ctx: &Context) -> Result<nix_bindings::EvalStats> {
        Ok(self.state.stats_with(ctx)?)
    }

    pub fn fetch_tree(&self, locked: &str, git_ssh_command: Option<&str>) -> Result<String> {
        // The pool reads nix's internal-json progress lines from stderr while this input downloads.
        self.ctx.set_log_format("internal-json")?;
        let _restore = SshCommand::set(git_ssh_command);
        let fetched = self.fetch_tree_out_path(locked);
        self.ctx.set_log_format("raw-with-logs")?;
        fetched
    }

    fn fetch_tree_out_path(&self, locked: &str) -> Result<String> {
        let fetch = self.state.eval_from_string(
            "json: (builtins.fetchTree (builtins.fromJSON json)).outPath",
            "/",
        )?;
        let json = self.state.make_string(locked)?;
        Ok(fetch.call(&json)?.as_string()?)
    }

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

struct SshCommand(bool);

impl SshCommand {
    fn set(command: Option<&str>) -> Self {
        let Some(command) = command else {
            return Self(false);
        };
        // SAFETY: the eval worker is serving this one request; its stats ticker never reads the environment.
        unsafe { std::env::set_var("GIT_SSH_COMMAND", command) };
        Self(true)
    }
}

impl Drop for SshCommand {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: see SshCommand::set.
            unsafe { std::env::remove_var("GIT_SSH_COMMAND") };
        }
    }
}
