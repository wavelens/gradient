/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Helpers for the `PATCH` endpoint pattern.
//!
//! Every mutating PATCH handler walks an `Option<T>` field on a request DTO
//! and, when `Some`, optionally validates and assigns to the matching
//! `ActiveModel` field. The two macros here capture the trivial and
//! validated shapes so handlers don't repeat 5–10 lines of boilerplate per
//! field.
//!
//! For complex per-field logic (cross-field validation, uniqueness checks
//! that must hit the DB, post-processing) keep the explicit `if let Some`
//! block — these helpers are deliberately scoped to the mechanical cases.

/// Set `active.$field = Set(value)` when `$body.$field` is `Some`.
///
/// ```ignore
/// patch_field!(aorg, body, description);
/// ```
#[macro_export]
macro_rules! patch_field {
    ($active:expr, $body:expr, $field:ident) => {
        if let Some(v) = $body.$field {
            $active.$field = ::sea_orm::Set(v);
        }
    };
}

/// Like [`patch_field!`] but applies `$transform` (a closure) to the value
/// before assigning. Useful for `.trim().to_string()` shaping.
///
/// ```ignore
/// patch_field_with!(aorg, body, description, |s: String| s.trim().to_string());
/// ```
#[macro_export]
macro_rules! patch_field_with {
    ($active:expr, $body:expr, $field:ident, $transform:expr) => {
        if let Some(v) = $body.$field {
            let f = $transform;
            $active.$field = ::sea_orm::Set(f(v));
        }
    };
}
