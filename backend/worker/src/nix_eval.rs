/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Nix expression evaluator backed by the Nix C API.
//!
//! Uses `nix_bindings::sys` (raw FFI) directly because the high-level
//! `nix-bindings` wrapper does not expose
//! `nix_flake_settings_add_to_eval_state_builder`, which is required to make
//! `builtins.getFlake` available — without it every flake-aware evaluation
//! fails with `nix eval failed` even though the same expression works via the
//! `nix` CLI.
//!
//! `nix_bindings` embeds Boehm GC into the process. Boehm GC cannot coexist
//! with Tokio's thread pool: it requires stop-the-world signal delivery to all
//! threads, but Tokio worker threads block those signals. Every method on
//! `NixEvaluator` is therefore **synchronous** and must be invoked from a
//! blocking context (e.g. `tokio::task::spawn_blocking`).

use anyhow::{Context as _, Result};
use nix_bindings::sys::{
    self, EvalState, Store, ValueType_NIX_TYPE_ATTRS, ValueType_NIX_TYPE_STRING, nix_alloc_value,
    nix_c_context, nix_c_context_create, nix_c_context_free, nix_err_NIX_OK, nix_err_msg,
    nix_eval_state_build, nix_eval_state_builder_free, nix_eval_state_builder_load,
    nix_eval_state_builder_new, nix_expr_eval_from_string, nix_flake_settings,
    nix_flake_settings_add_to_eval_state_builder, nix_flake_settings_free, nix_flake_settings_new,
    nix_get_attr_byidx, nix_get_attrs_size, nix_get_string, nix_get_type, nix_libexpr_init,
    nix_libstore_init, nix_libutil_init, nix_state_free, nix_store_free, nix_store_open, nix_value,
    nix_value_force,
};
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_uint, c_void};
use std::ptr;

// ---------------------------------------------------------------------------
// NixEvaluator
// ---------------------------------------------------------------------------

/// Evaluates Nix expressions through the embedded Nix C API.
///
/// Create one instance per evaluation session. All methods are **synchronous**
/// and must be called from a blocking context (e.g. `tokio::task::spawn_blocking`).
pub struct NixEvaluator {
    ctx: *mut nix_c_context,
    store: *mut Store,
    flake_settings: *mut nix_flake_settings,
    state: *mut EvalState,
}

// SAFETY: NixEvaluator is only used from one thread at a time (spawn_blocking).
// All FFI calls are serialized through the &self/&mut self methods.
unsafe impl Send for NixEvaluator {}
unsafe impl Sync for NixEvaluator {}

impl NixEvaluator {
    pub fn new() -> Result<Self> {
        unsafe {
            let ctx = nix_c_context_create();
            if ctx.is_null() {
                anyhow::bail!("nix_c_context_create returned null");
            }

            check(ctx, nix_libutil_init(ctx)).context("nix_libutil_init")?;
            check(ctx, nix_libstore_init(ctx)).context("nix_libstore_init")?;
            check(ctx, nix_libexpr_init(ctx)).context("nix_libexpr_init")?;

            // NULL uri → use the store from ambient settings (same as `nix eval`).
            let store = nix_store_open(ctx, ptr::null(), ptr::null_mut());
            if store.is_null() {
                let msg = err_msg(ctx);
                nix_c_context_free(ctx);
                anyhow::bail!("nix_store_open failed: {}", msg);
            }

            let builder = nix_eval_state_builder_new(ctx, store);
            if builder.is_null() {
                let msg = err_msg(ctx);
                nix_store_free(store);
                nix_c_context_free(ctx);
                anyhow::bail!("nix_eval_state_builder_new returned null: {}", msg);
            }

            if let Err(e) = check(ctx, nix_eval_state_builder_load(ctx, builder)) {
                nix_eval_state_builder_free(builder);
                nix_store_free(store);
                nix_c_context_free(ctx);
                return Err(e).context("nix_eval_state_builder_load");
            }

            // Register flake support so that `builtins.getFlake` is available.
            let flake_settings = nix_flake_settings_new(ctx);
            if flake_settings.is_null() {
                let msg = err_msg(ctx);
                nix_eval_state_builder_free(builder);
                nix_store_free(store);
                nix_c_context_free(ctx);
                anyhow::bail!("nix_flake_settings_new returned null: {}", msg);
            }

            if let Err(e) = check(
                ctx,
                nix_flake_settings_add_to_eval_state_builder(ctx, flake_settings, builder),
            ) {
                nix_flake_settings_free(flake_settings);
                nix_eval_state_builder_free(builder);
                nix_store_free(store);
                nix_c_context_free(ctx);
                return Err(e).context("nix_flake_settings_add_to_eval_state_builder");
            }

            // nix_eval_state_build takes ownership of `builder` and frees it.
            let state = nix_eval_state_build(ctx, builder);
            if state.is_null() {
                let msg = err_msg(ctx);
                nix_flake_settings_free(flake_settings);
                nix_store_free(store);
                nix_c_context_free(ctx);
                anyhow::bail!("nix_eval_state_build returned null: {}", msg);
            }

            Ok(NixEvaluator {
                ctx,
                store,
                flake_settings,
                state,
            })
        }
    }

    /// Evaluate `expr` as an attrset and return its attribute names.
    pub fn attr_names(&self, expr: &str) -> Result<Vec<String>> {
        unsafe {
            let value = self.eval(expr)?;
            if nix_get_type(self.ctx, value) != ValueType_NIX_TYPE_ATTRS {
                anyhow::bail!("expected attrset from: {}", expr);
            }

            let n = nix_get_attrs_size(self.ctx, value);
            let mut names = Vec::with_capacity(n as usize);
            for i in 0..n {
                let mut name_ptr: *const c_char = ptr::null();
                let _attr = nix_get_attr_byidx(self.ctx, value, self.state, i, &mut name_ptr);
                if !name_ptr.is_null() {
                    names.push(CStr::from_ptr(name_ptr).to_string_lossy().into_owned());
                }
            }
            Ok(names)
        }
    }

    /// Evaluate `expr` and return it as a string.
    pub fn eval_string(&self, expr: &str) -> Result<String> {
        unsafe {
            let value = self.eval(expr)?;
            if nix_get_type(self.ctx, value) != ValueType_NIX_TYPE_STRING {
                anyhow::bail!("expected string from: {}", expr);
            }

            let mut out = String::new();
            check(
                self.ctx,
                nix_get_string(
                    self.ctx,
                    value,
                    Some(string_receiver),
                    &mut out as *mut String as *mut c_void,
                ),
            )
            .with_context(|| format!("nix_get_string failed: {}", expr))?;
            Ok(out)
        }
    }

    /// Evaluate `expr` and return a forced value pointer. The pointer is owned
    /// by the eval state and remains valid until the next eval call or drop.
    unsafe fn eval(&self, expr: &str) -> Result<*mut nix_value> {
        let expr_c = CString::new(expr).context("expr contains a null byte")?;
        let path_c = CString::new("<gradient>").unwrap();

        unsafe {
            let value = nix_alloc_value(self.ctx, self.state);
            if value.is_null() {
                anyhow::bail!("nix_alloc_value returned null: {}", err_msg(self.ctx));
            }

            check(
                self.ctx,
                nix_expr_eval_from_string(
                    self.ctx,
                    self.state,
                    expr_c.as_ptr(),
                    path_c.as_ptr(),
                    value,
                ),
            )
            .with_context(|| format!("nix eval failed: {}", expr))?;

            check(self.ctx, nix_value_force(self.ctx, self.state, value))
                .with_context(|| format!("nix force failed: {}", expr))?;

            Ok(value)
        }
    }
}

impl Drop for NixEvaluator {
    fn drop(&mut self) {
        unsafe {
            nix_state_free(self.state);
            nix_flake_settings_free(self.flake_settings);
            nix_store_free(self.store);
            nix_c_context_free(self.ctx);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

unsafe extern "C" fn string_receiver(start: *const c_char, n: c_uint, user_data: *mut c_void) {
    unsafe {
        let bytes = std::slice::from_raw_parts(start as *const u8, n as usize);
        let buf = &mut *(user_data as *mut String);
        buf.push_str(&String::from_utf8_lossy(bytes));
    }
}

fn check(ctx: *mut nix_c_context, err: sys::nix_err) -> Result<()> {
    if err == nix_err_NIX_OK as sys::nix_err {
        return Ok(());
    }
    let msg = unsafe { err_msg(ctx) };
    Err(anyhow::anyhow!("nix error (code {}): {}", err, msg))
}

unsafe fn err_msg(ctx: *mut nix_c_context) -> String {
    unsafe {
        let mut len: c_uint = 0;
        let ptr = nix_err_msg(ptr::null_mut(), ctx, &mut len);
        if ptr.is_null() || len == 0 {
            return "unknown error".to_string();
        }
        let bytes = std::slice::from_raw_parts(ptr as *const u8, len as usize);
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Escape a string for embedding inside a Nix double-quoted string literal.
pub fn escape_nix_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
