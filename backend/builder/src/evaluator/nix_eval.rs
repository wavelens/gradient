/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Safe wrapper around the Nix C API for evaluating flake attribute trees.
//!
//! All methods are **synchronous** and must be called from a blocking context
//! (e.g. inside `tokio::task::spawn_blocking`).

use anyhow::{Context, Result};
use nix_bindings::sys::*;
use std::ffi::{CStr, CString};
use std::mem::MaybeUninit;
use std::os::raw::{c_char, c_uint, c_void};

// ---------------------------------------------------------------------------
// String callback (must be a named extern "C" fn, not a closure)
// ---------------------------------------------------------------------------

unsafe extern "C" fn string_receiver(
    start: *const c_char,
    n: c_uint,
    user_data: *mut c_void,
) {
    unsafe {
        let bytes = std::slice::from_raw_parts(start as *const u8, n as usize);
        let buf = &mut *(user_data as *mut String);
        buf.push_str(&String::from_utf8_lossy(bytes));
    }
}

// ---------------------------------------------------------------------------
// NixEvaluator
// ---------------------------------------------------------------------------

/// Holds a live Nix evaluation context.
///
/// Create one per logical evaluation session (e.g. one per flake discovery
/// run).  All methods are blocking; call them from `spawn_blocking`.
pub struct NixEvaluator {
    ctx: *mut nix_c_context,
    store: *mut Store,
    flake_settings: *mut nix_flake_settings,
    state: *mut EvalState,
}

// Safety: NixEvaluator is only accessed from one thread at a time (spawn_blocking).
unsafe impl Send for NixEvaluator {}

impl NixEvaluator {
    /// Initialise the Nix evaluator and connect to the local store.
    pub fn new() -> Result<Self> {
        unsafe {
            let ctx = nix_c_context_create();
            if ctx.is_null() {
                anyhow::bail!("nix_c_context_create returned null");
            }

            // Silence nix log output; NIX_LVL_ERROR = 0
            nix_set_verbosity(ctx, 0);

            nix_check(ctx, nix_libutil_init(ctx)).context("nix_libutil_init")?;
            nix_check(ctx, nix_libstore_init(ctx)).context("nix_libstore_init")?;
            nix_check(ctx, nix_libexpr_init(ctx)).context("nix_libexpr_init")?;

            let flake_settings = nix_flake_settings_new(ctx);
            if flake_settings.is_null() {
                anyhow::bail!("nix_flake_settings_new returned null: {}", err_msg(ctx));
            }

            // NULL uri → use the store from ambient settings (same as `nix eval`)
            let store = nix_store_open(ctx, std::ptr::null(), std::ptr::null_mut());
            if store.is_null() {
                anyhow::bail!("nix_store_open failed: {}", err_msg(ctx));
            }

            let builder = nix_eval_state_builder_new(ctx, store);
            if builder.is_null() {
                anyhow::bail!("nix_eval_state_builder_new returned null: {}", err_msg(ctx));
            }

            nix_check(ctx, nix_eval_state_builder_load(ctx, builder))
                .context("nix_eval_state_builder_load")?;

            nix_check(
                ctx,
                nix_flake_settings_add_to_eval_state_builder(ctx, flake_settings, builder),
            )
            .context("nix_flake_settings_add_to_eval_state_builder")?;

            let state = nix_eval_state_build(ctx, builder);
            nix_eval_state_builder_free(builder);
            if state.is_null() {
                anyhow::bail!("nix_eval_state_build returned null: {}", err_msg(ctx));
            }

            Ok(NixEvaluator { ctx, store, flake_settings, state })
        }
    }

    /// Evaluate `expr`, force the result, and return the attribute names of
    /// the resulting attrset.  Returns `Err` if the expression fails (e.g.
    /// the attribute path does not exist in the flake).
    pub fn attr_names(&self, expr: &str) -> Result<Vec<String>> {
        unsafe {
            let expr_c = CString::new(expr).context("expr contains a null byte")?;
            let path_c = CString::new("/").unwrap();

            let mut value = MaybeUninit::<nix_value>::uninit();

            nix_check(
                self.ctx,
                nix_expr_eval_from_string(
                    self.ctx,
                    self.state,
                    expr_c.as_ptr(),
                    path_c.as_ptr(),
                    value.as_mut_ptr(),
                ),
            )
            .with_context(|| format!("nix eval failed: {}", expr))?;

            nix_check(self.ctx, nix_value_force(self.ctx, self.state, value.as_mut_ptr()))
                .with_context(|| format!("nix force failed: {}", expr))?;

            if nix_get_type(self.ctx, value.as_mut_ptr()) != ValueType_NIX_TYPE_ATTRS {
                anyhow::bail!("expected attrset from: {}", expr);
            }

            let n = nix_get_attrs_size(self.ctx, value.as_mut_ptr());
            let mut names = Vec::with_capacity(n as usize);
            for i in 0..n {
                let mut name_ptr: *const c_char = std::ptr::null();
                let _attr_val =
                    nix_get_attr_byidx(self.ctx, value.as_mut_ptr(), self.state, i, &mut name_ptr);
                if !name_ptr.is_null() {
                    names.push(CStr::from_ptr(name_ptr).to_string_lossy().into_owned());
                }
            }
            Ok(names)
        }
    }

    /// Evaluate `expr`, force the result, and return its string value.
    /// Returns `Err` if the expression fails or the result is not a string.
    pub fn eval_string(&self, expr: &str) -> Result<String> {
        unsafe {
            let expr_c = CString::new(expr).context("expr contains a null byte")?;
            let path_c = CString::new("/").unwrap();

            let mut value = MaybeUninit::<nix_value>::uninit();

            nix_check(
                self.ctx,
                nix_expr_eval_from_string(
                    self.ctx,
                    self.state,
                    expr_c.as_ptr(),
                    path_c.as_ptr(),
                    value.as_mut_ptr(),
                ),
            )
            .with_context(|| format!("nix eval failed: {}", expr))?;

            nix_check(self.ctx, nix_value_force(self.ctx, self.state, value.as_mut_ptr()))
                .with_context(|| format!("nix force failed: {}", expr))?;

            if nix_get_type(self.ctx, value.as_mut_ptr()) != ValueType_NIX_TYPE_STRING {
                anyhow::bail!("expected string from: {}", expr);
            }

            let mut result = String::new();
            nix_check(
                self.ctx,
                nix_get_string(
                    self.ctx,
                    value.as_mut_ptr(),
                    Some(string_receiver),
                    &mut result as *mut String as *mut c_void,
                ),
            )
            .with_context(|| format!("nix_get_string failed: {}", expr))?;

            Ok(result)
        }
    }
}

impl Drop for NixEvaluator {
    fn drop(&mut self) {
        unsafe {
            nix_state_free(self.state);
            nix_store_free(self.store);
            nix_flake_settings_free(self.flake_settings);
            nix_c_context_free(self.ctx);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn nix_check(ctx: *mut nix_c_context, err: nix_err) -> Result<()> {
    if err == nix_err_NIX_OK {
        return Ok(());
    }
    Err(anyhow::anyhow!("nix error (code {}): {}", err, unsafe { err_msg(ctx) }))
}

unsafe fn err_msg(ctx: *mut nix_c_context) -> String {
    unsafe {
        let ptr = nix_err_msg(std::ptr::null_mut(), ctx, std::ptr::null_mut());
        if ptr.is_null() {
            "unknown error".to_string()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}

/// Escape a string for embedding inside a Nix double-quoted string literal.
pub fn escape_nix_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
