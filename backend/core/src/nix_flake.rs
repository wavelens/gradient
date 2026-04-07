/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Direct FFI wrapper around `nix_flake_lock` to fetch all flake inputs into
//! the local Nix store without shelling out to the `nix` CLI.
//!
//! Provides a single entry point [`lock_flake_with_ssh_key`] which:
//!   1. Writes the supplied SSH private key to a mode-600 temp file.
//!   2. Sets `GIT_SSH_COMMAND` so that git invocations from libfetchers pick
//!      up that key.
//!   3. Initializes a Nix evaluation context with flake settings enabled.
//!   4. Calls `nix_flake_lock` against `path:<flake_dir>`, which fetches every
//!      transitive input into `/nix/store`.
//!   5. Cleans up all FFI handles, the temp key, and restores `GIT_SSH_COMMAND`.
//!
//! The whole operation runs serialized behind a global mutex because both the
//! Nix C API and `GIT_SSH_COMMAND` (a process-global env var) are not safe to
//! drive concurrently.

use nix_bindings::sys::*;
use std::ffi::{CString, c_char, c_uint, c_void};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::ptr;
use std::sync::Mutex;
use tempfile::NamedTempFile;

static FLAKE_LOCK_LOCK: Mutex<()> = Mutex::new(());

/// Errors returned by [`lock_flake_with_ssh_key`].
#[derive(Debug, thiserror::Error)]
pub enum FlakeLockError {
    #[error("nix flake locking failed: {0}")]
    Nix(String),
    #[error("failed to write SSH key file: {0}")]
    KeyFile(String),
}

/// Lock the flake at `flake_dir`, fetching every transitive input into the
/// store. `ssh_private_key` is used for any `git+ssh` inputs.
pub fn lock_flake_with_ssh_key(
    flake_dir: &Path,
    ssh_private_key: &str,
) -> Result<(), FlakeLockError> {
    let _guard = FLAKE_LOCK_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Write the SSH key to a temp file with mode 0600.
    let key_file = NamedTempFile::with_suffix(".key")
        .map_err(|e| FlakeLockError::KeyFile(e.to_string()))?;
    fs::set_permissions(key_file.path(), fs::Permissions::from_mode(0o600))
        .map_err(|e| FlakeLockError::KeyFile(e.to_string()))?;
    fs::write(key_file.path(), ssh_private_key.as_bytes())
        .map_err(|e| FlakeLockError::KeyFile(e.to_string()))?;

    let ssh_command = format!(
        "ssh -i {} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o IdentitiesOnly=yes",
        key_file.path().display()
    );

    // SAFETY: env var mutation is process-global, but we hold FLAKE_LOCK_LOCK
    // for the entire duration so no other thread is reading or writing it.
    let prev_ssh_command = std::env::var_os("GIT_SSH_COMMAND");
    unsafe { std::env::set_var("GIT_SSH_COMMAND", &ssh_command) };

    let result = unsafe { lock_flake_inner(flake_dir) };

    // Restore the prior env var (or unset it).
    unsafe {
        match prev_ssh_command {
            Some(prev) => std::env::set_var("GIT_SSH_COMMAND", prev),
            None => std::env::remove_var("GIT_SSH_COMMAND"),
        }
    }
    drop(key_file);

    result
}

/// Walks the entire FFI initialization → lock → cleanup sequence.
unsafe fn lock_flake_inner(flake_dir: &Path) -> Result<(), FlakeLockError> {
    unsafe {
        let ctx = nix_c_context_create();
        if ctx.is_null() {
            return Err(FlakeLockError::Nix("nix_c_context_create returned null".into()));
        }

        // Use a scope-guard pattern via Drop is verbose with raw pointers; use
        // a closure + explicit cleanup labels instead.
        let result = (|| -> Result<(), FlakeLockError> {
            check(ctx, nix_libutil_init(ctx), "nix_libutil_init")?;
            check(ctx, nix_libstore_init(ctx), "nix_libstore_init")?;
            check(ctx, nix_libexpr_init(ctx), "nix_libexpr_init")?;

            let store = nix_store_open(ctx, ptr::null(), ptr::null_mut());
            if store.is_null() {
                return Err(nix_error(ctx, "nix_store_open"));
            }

            let fetchers_settings = nix_fetchers_settings_new(ctx);
            if fetchers_settings.is_null() {
                nix_store_free(store);
                return Err(nix_error(ctx, "nix_fetchers_settings_new"));
            }

            let flake_settings = nix_flake_settings_new(ctx);
            if flake_settings.is_null() {
                nix_fetchers_settings_free(fetchers_settings);
                nix_store_free(store);
                return Err(nix_error(ctx, "nix_flake_settings_new"));
            }

            let builder = nix_eval_state_builder_new(ctx, store);
            if builder.is_null() {
                nix_flake_settings_free(flake_settings);
                nix_fetchers_settings_free(fetchers_settings);
                nix_store_free(store);
                return Err(nix_error(ctx, "nix_eval_state_builder_new"));
            }

            let inner = (|| -> Result<(), FlakeLockError> {
                check(
                    ctx,
                    nix_eval_state_builder_load(ctx, builder),
                    "nix_eval_state_builder_load",
                )?;
                check(
                    ctx,
                    nix_flake_settings_add_to_eval_state_builder(ctx, flake_settings, builder),
                    "nix_flake_settings_add_to_eval_state_builder",
                )?;

                let state = nix_eval_state_build(ctx, builder);
                if state.is_null() {
                    return Err(nix_error(ctx, "nix_eval_state_build"));
                }

                let state_result = (|| -> Result<(), FlakeLockError> {
                    let parse_flags =
                        nix_flake_reference_parse_flags_new(ctx, flake_settings);
                    if parse_flags.is_null() {
                        return Err(nix_error(ctx, "nix_flake_reference_parse_flags_new"));
                    }

                    let parse_result = (|| -> Result<(), FlakeLockError> {
                        let url = format!("path:{}", flake_dir.display());
                        let url_c = CString::new(url)
                            .map_err(|e| FlakeLockError::Nix(format!("flake url: {}", e)))?;

                        let mut flake_ref: *mut nix_flake_reference = ptr::null_mut();
                        check(
                            ctx,
                            nix_flake_reference_and_fragment_from_string(
                                ctx,
                                fetchers_settings,
                                flake_settings,
                                parse_flags,
                                url_c.as_ptr(),
                                url_c.as_bytes().len(),
                                &mut flake_ref,
                                Some(noop_string_cb),
                                ptr::null_mut(),
                            ),
                            "nix_flake_reference_and_fragment_from_string",
                        )?;
                        if flake_ref.is_null() {
                            return Err(nix_error(ctx, "flake reference is null"));
                        }

                        let ref_result = (|| -> Result<(), FlakeLockError> {
                            let lock_flags = nix_flake_lock_flags_new(ctx, flake_settings);
                            if lock_flags.is_null() {
                                return Err(nix_error(ctx, "nix_flake_lock_flags_new"));
                            }

                            let locked = nix_flake_lock(
                                ctx,
                                fetchers_settings,
                                flake_settings,
                                state,
                                lock_flags,
                                flake_ref,
                            );
                            let lock_result = if locked.is_null() {
                                Err(nix_error(ctx, "nix_flake_lock"))
                            } else {
                                nix_locked_flake_free(locked);
                                Ok(())
                            };
                            nix_flake_lock_flags_free(lock_flags);
                            lock_result
                        })();

                        nix_flake_reference_free(flake_ref);
                        ref_result
                    })();

                    nix_flake_reference_parse_flags_free(parse_flags);
                    parse_result
                })();

                nix_state_free(state);
                state_result
            })();

            nix_eval_state_builder_free(builder);
            nix_flake_settings_free(flake_settings);
            nix_fetchers_settings_free(fetchers_settings);
            nix_store_free(store);
            inner
        })();

        nix_c_context_free(ctx);
        result
    }
}

/// Convert a non-OK error code into a [`FlakeLockError::Nix`] with the
/// human-readable Nix error message attached.
unsafe fn check(
    ctx: *mut nix_c_context,
    err: nix_err,
    op: &'static str,
) -> Result<(), FlakeLockError> {
    if err == nix_err_NIX_OK {
        Ok(())
    } else {
        Err(unsafe { nix_error(ctx, op) })
    }
}

unsafe fn nix_error(ctx: *mut nix_c_context, op: &str) -> FlakeLockError {
    let mut msg: Option<String> = None;
    unsafe {
        nix_err_info_msg(
            ptr::null_mut(),
            ctx,
            Some(collect_string_cb),
            (&mut msg as *mut Option<String>).cast::<c_void>(),
        );
    }
    let msg = msg.unwrap_or_else(|| "no error info".to_string());
    FlakeLockError::Nix(format!("{}: {}", op, msg))
}

extern "C" fn noop_string_cb(_start: *const c_char, _n: c_uint, _user_data: *mut c_void) {}

extern "C" fn collect_string_cb(start: *const c_char, n: c_uint, user_data: *mut c_void) {
    if start.is_null() || user_data.is_null() {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(start.cast::<u8>(), n as usize) };
    let s = String::from_utf8_lossy(bytes).into_owned();
    let out = user_data.cast::<Option<String>>();
    unsafe { *out = Some(s) };
}
