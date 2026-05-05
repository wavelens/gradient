/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Compile-fail proofs that typed IDs reject argument swaps. If the rustc
//! diagnostic ever changes, regenerate the .stderr files via:
//!     TRYBUILD=overwrite cargo test -p entity --test compile_fail

#[test]
fn swap_user_for_org_id_is_a_compile_error() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/swap_user_for_org.rs");
}
