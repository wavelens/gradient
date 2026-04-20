/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Tests for `core::executer::path_utils` — pure string manipulation.

extern crate core as gradient_core;
use gradient_core::executer::path_utils::{
    nix_store_path, strip_nix_store_prefix, strip_store_prefix,
};

// ── nix_store_path ───────────────────────────────────────────────────────────

#[test]
fn nix_store_path_prepends_prefix_to_bare_hash_name() {
    assert_eq!(
        nix_store_path("abc123-foo"),
        "/nix/store/abc123-foo"
    );
}

#[test]
fn nix_store_path_passes_through_already_absolute_path() {
    assert_eq!(
        nix_store_path("/nix/store/abc123-foo"),
        "/nix/store/abc123-foo"
    );
}

#[test]
fn nix_store_path_passes_through_other_absolute_paths() {
    // Any path starting with `/` is left alone — the function does not
    // re-check that the prefix is actually `/nix/store/`.
    assert_eq!(nix_store_path("/tmp/foo"), "/tmp/foo");
}

#[test]
fn nix_store_path_empty_input_gets_prefix() {
    assert_eq!(nix_store_path(""), "/nix/store/");
}

// ── strip_nix_store_prefix ───────────────────────────────────────────────────

#[test]
fn strip_nix_store_prefix_removes_prefix() {
    assert_eq!(
        strip_nix_store_prefix("/nix/store/abc123-foo"),
        "abc123-foo"
    );
}

#[test]
fn strip_nix_store_prefix_passthrough_when_missing() {
    assert_eq!(strip_nix_store_prefix("abc123-foo"), "abc123-foo");
    assert_eq!(strip_nix_store_prefix("/tmp/foo"), "/tmp/foo");
}

#[test]
fn strip_nix_store_prefix_empty_stays_empty() {
    assert_eq!(strip_nix_store_prefix(""), "");
}

// ── strip_store_prefix ───────────────────────────────────────────────────────

#[test]
fn strip_store_prefix_removes_prefix_without_allocating() {
    // Return type is `&str`, so slicing into the input is confirmed by identity.
    let input = "/nix/store/abc123-foo";
    let out = strip_store_prefix(input);
    assert_eq!(out, "abc123-foo");
    assert!(std::ptr::eq(out.as_ptr(), input[11..].as_ptr()));
}

#[test]
fn strip_store_prefix_passthrough_when_missing() {
    let input = "abc123-foo";
    let out = strip_store_prefix(input);
    assert_eq!(out, "abc123-foo");
    assert!(std::ptr::eq(out.as_ptr(), input.as_ptr()));
}
