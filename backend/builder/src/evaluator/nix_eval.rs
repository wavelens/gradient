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
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_uint, c_void};

// ---------------------------------------------------------------------------
// Raw FFI declarations
// ---------------------------------------------------------------------------

mod sys {
    use std::os::raw::{c_char, c_int, c_uint, c_void};

    pub type NixErr = c_int;
    pub const NIX_OK: NixErr = 0;

    pub type NixGetStringCallback =
        unsafe extern "C" fn(start: *const c_char, n: c_uint, user_data: *mut c_void);

    // Opaque C types
    #[repr(C)]
    pub struct NixCContext {
        _opaque: [u8; 0],
    }
    #[repr(C)]
    pub struct Store {
        _opaque: [u8; 0],
    }
    #[repr(C)]
    pub struct EvalState {
        _opaque: [u8; 0],
    }
    #[repr(C)]
    pub struct NixValue {
        _opaque: [u8; 0],
    }
    #[repr(C)]
    pub struct NixEvalStateBuilder {
        _opaque: [u8; 0],
    }
    #[repr(C)]
    pub struct NixFlakeSettings {
        _opaque: [u8; 0],
    }

    #[repr(C)]
    #[derive(Debug, PartialEq, Eq)]
    #[allow(dead_code)]
    pub enum ValueType {
        Thunk = 0,
        Int = 1,
        Float = 2,
        Bool = 3,
        String = 4,
        Path = 5,
        Null = 6,
        Attrs = 7,
        List = 8,
        Function = 9,
        External = 10,
    }

    unsafe extern "C" {
        // Context
        pub fn nix_c_context_create() -> *mut NixCContext;
        pub fn nix_c_context_free(ctx: *mut NixCContext);
        pub fn nix_err_msg(
            ctx: *mut NixCContext,
            read_ctx: *const NixCContext,
            n: *mut c_uint,
        ) -> *const c_char;

        // Init
        pub fn nix_libutil_init(ctx: *mut NixCContext) -> NixErr;
        pub fn nix_libstore_init(ctx: *mut NixCContext) -> NixErr;
        pub fn nix_libexpr_init(ctx: *mut NixCContext) -> NixErr;
        pub fn nix_set_verbosity(ctx: *mut NixCContext, level: c_int) -> NixErr;

        // Store
        pub fn nix_store_open(
            ctx: *mut NixCContext,
            uri: *const c_char,
            params: *mut *mut *mut c_char,
        ) -> *mut Store;
        pub fn nix_store_free(store: *mut Store);

        // EvalState builder
        pub fn nix_eval_state_builder_new(
            ctx: *mut NixCContext,
            store: *mut Store,
        ) -> *mut NixEvalStateBuilder;
        #[allow(dead_code)]
        pub fn nix_eval_state_builder_free(builder: *mut NixEvalStateBuilder);
        pub fn nix_eval_state_builder_load(
            ctx: *mut NixCContext,
            builder: *mut NixEvalStateBuilder,
        ) -> NixErr;
        pub fn nix_eval_state_build(
            ctx: *mut NixCContext,
            builder: *mut NixEvalStateBuilder,
        ) -> *mut EvalState;
        pub fn nix_state_free(state: *mut EvalState);

        // Flake settings
        pub fn nix_flake_settings_new(ctx: *mut NixCContext) -> *mut NixFlakeSettings;
        pub fn nix_flake_settings_free(settings: *mut NixFlakeSettings);
        pub fn nix_flake_settings_add_to_eval_state_builder(
            ctx: *mut NixCContext,
            settings: *mut NixFlakeSettings,
            builder: *mut NixEvalStateBuilder,
        ) -> NixErr;

        // Values
        pub fn nix_alloc_value(ctx: *mut NixCContext, state: *mut EvalState) -> *mut NixValue;
        pub fn nix_gc_decref(ctx: *mut NixCContext, obj: *const c_void) -> NixErr;

        // Evaluation
        pub fn nix_expr_eval_from_string(
            ctx: *mut NixCContext,
            state: *mut EvalState,
            expr: *const c_char,
            path: *const c_char,
            value: *mut NixValue,
        ) -> NixErr;
        pub fn nix_value_force(
            ctx: *mut NixCContext,
            state: *mut EvalState,
            value: *mut NixValue,
        ) -> NixErr;

        // Inspection
        pub fn nix_get_type(ctx: *mut NixCContext, value: *const NixValue) -> ValueType;
        pub fn nix_get_attrs_size(ctx: *mut NixCContext, value: *const NixValue) -> c_uint;
        pub fn nix_get_attr_name_byidx(
            ctx: *mut NixCContext,
            value: *mut NixValue,
            state: *mut EvalState,
            i: c_uint,
        ) -> *const c_char;
        pub fn nix_get_string(
            ctx: *mut NixCContext,
            value: *const NixValue,
            callback: NixGetStringCallback,
            user_data: *mut c_void,
        ) -> NixErr;
    }
}

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
// RAII guard: decref a nix_value when dropped
// ---------------------------------------------------------------------------

struct ValueGuard {
    ctx: *mut sys::NixCContext,
    value: *mut sys::NixValue,
}

impl Drop for ValueGuard {
    fn drop(&mut self) {
        unsafe {
            sys::nix_gc_decref(self.ctx, self.value as *const c_void);
        }
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
    ctx: *mut sys::NixCContext,
    store: *mut sys::Store,
    flake_settings: *mut sys::NixFlakeSettings,
    state: *mut sys::EvalState,
}

// Safety: NixEvaluator is only accessed from one thread at a time (spawn_blocking).
unsafe impl Send for NixEvaluator {}

impl NixEvaluator {
    /// Initialise the Nix evaluator and connect to the local store.
    pub fn new() -> Result<Self> {
        unsafe {
            let ctx = sys::nix_c_context_create();
            if ctx.is_null() {
                anyhow::bail!("nix_c_context_create returned null");
            }

            // Silence nix log output; NIX_LVL_ERROR = 0
            sys::nix_set_verbosity(ctx, 0);

            nix_check(ctx, sys::nix_libutil_init(ctx)).context("nix_libutil_init")?;
            nix_check(ctx, sys::nix_libstore_init(ctx)).context("nix_libstore_init")?;
            nix_check(ctx, sys::nix_libexpr_init(ctx)).context("nix_libexpr_init")?;

            let flake_settings = sys::nix_flake_settings_new(ctx);
            if flake_settings.is_null() {
                anyhow::bail!("nix_flake_settings_new returned null: {}", err_msg(ctx));
            }

            // NULL uri → use the store from ambient settings (same as `nix eval`)
            let store = sys::nix_store_open(ctx, std::ptr::null(), std::ptr::null_mut());
            if store.is_null() {
                anyhow::bail!("nix_store_open failed: {}", err_msg(ctx));
            }

            let builder = sys::nix_eval_state_builder_new(ctx, store);
            if builder.is_null() {
                anyhow::bail!("nix_eval_state_builder_new returned null: {}", err_msg(ctx));
            }

            nix_check(ctx, sys::nix_eval_state_builder_load(ctx, builder))
                .context("nix_eval_state_builder_load")?;

            nix_check(
                ctx,
                sys::nix_flake_settings_add_to_eval_state_builder(ctx, flake_settings, builder),
            )
            .context("nix_flake_settings_add_to_eval_state_builder")?;

            // nix_eval_state_build takes ownership of `builder` and frees it
            let state = sys::nix_eval_state_build(ctx, builder);
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

            let value = sys::nix_alloc_value(self.ctx, self.state);
            if value.is_null() {
                anyhow::bail!("nix_alloc_value returned null");
            }
            let _guard = ValueGuard { ctx: self.ctx, value };

            nix_check(
                self.ctx,
                sys::nix_expr_eval_from_string(
                    self.ctx,
                    self.state,
                    expr_c.as_ptr(),
                    path_c.as_ptr(),
                    value,
                ),
            )
            .with_context(|| format!("nix eval failed: {}", expr))?;

            nix_check(self.ctx, sys::nix_value_force(self.ctx, self.state, value))
                .with_context(|| format!("nix force failed: {}", expr))?;

            if sys::nix_get_type(self.ctx, value) != sys::ValueType::Attrs {
                anyhow::bail!("expected attrset from: {}", expr);
            }

            let n = sys::nix_get_attrs_size(self.ctx, value);
            let mut names = Vec::with_capacity(n as usize);
            for i in 0..n {
                let name_ptr =
                    sys::nix_get_attr_name_byidx(self.ctx, value, self.state, i);
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

            let value = sys::nix_alloc_value(self.ctx, self.state);
            if value.is_null() {
                anyhow::bail!("nix_alloc_value returned null");
            }
            let _guard = ValueGuard { ctx: self.ctx, value };

            nix_check(
                self.ctx,
                sys::nix_expr_eval_from_string(
                    self.ctx,
                    self.state,
                    expr_c.as_ptr(),
                    path_c.as_ptr(),
                    value,
                ),
            )
            .with_context(|| format!("nix eval failed: {}", expr))?;

            nix_check(self.ctx, sys::nix_value_force(self.ctx, self.state, value))
                .with_context(|| format!("nix force failed: {}", expr))?;

            if sys::nix_get_type(self.ctx, value) != sys::ValueType::String {
                anyhow::bail!("expected string from: {}", expr);
            }

            let mut result = String::new();
            nix_check(
                self.ctx,
                sys::nix_get_string(
                    self.ctx,
                    value,
                    string_receiver,
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
            sys::nix_state_free(self.state);
            sys::nix_store_free(self.store);
            sys::nix_flake_settings_free(self.flake_settings);
            sys::nix_c_context_free(self.ctx);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn nix_check(ctx: *mut sys::NixCContext, err: sys::NixErr) -> Result<()> {
    if err == sys::NIX_OK {
        return Ok(());
    }
    Err(anyhow::anyhow!("nix error (code {}): {}", err, unsafe { err_msg(ctx) }))
}

unsafe fn err_msg(ctx: *mut sys::NixCContext) -> String {
    unsafe {
        let ptr = sys::nix_err_msg(std::ptr::null_mut(), ctx, std::ptr::null_mut());
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
