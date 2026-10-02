/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[macro_export]
macro_rules! patch_field {
    ($active:expr, $body:expr, $field:ident) => {
        if let Some(v) = $body.$field {
            $active.$field = ::sea_orm::Set(v);
        }
    };
}

#[macro_export]
macro_rules! patch_field_with {
    ($active:expr, $body:expr, $field:ident, $transform:expr) => {
        if let Some(v) = $body.$field {
            let f = $transform;
            $active.$field = ::sea_orm::Set(f(v));
        }
    };
}
