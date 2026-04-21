/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `core::storage::nar_extract`.
//!
//! The `core` crate name shadows stdlib `::core`, which breaks `#[tokio::test]`
//! macro expansion (it emits `::core::pin::Pin`, `::core::future::Future`).
//! We therefore use sync `#[test]` + `tokio::runtime::Builder::block_on`.

extern crate core as gradient_core;

use bytes::Bytes;
use gradient_core::storage::nar_extract::{ExtractError, extract_file_from_nar_bytes};
use harmonia_nar::archive::write_nar;
use harmonia_nar::archive::test_data::{TestNarEvent, TestNarEvents};

fn dir_with_file(name: &str, contents: &[u8], executable: bool) -> Vec<u8> {
    let events: TestNarEvents = vec![
        TestNarEvent::StartDirectory { name: Bytes::new() },
        TestNarEvent::File {
            name: Bytes::from(name.to_owned().into_bytes()),
            executable,
            size: contents.len() as u64,
            reader: std::io::Cursor::new(Bytes::from(contents.to_vec())),
        },
        TestNarEvent::EndDirectory,
    ];
    write_nar(&events).to_vec()
}

fn zstd_compress(data: &[u8]) -> Vec<u8> {
    zstd::encode_all(std::io::Cursor::new(data), 3).unwrap()
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

#[test]
fn extracts_file_at_relative_path() {
    let nar = dir_with_file("hello.txt", b"hi there", false);
    let compressed = zstd_compress(&nar);

    let out = block_on(extract_file_from_nar_bytes(compressed, "hello.txt")).unwrap();
    assert_eq!(out.contents, b"hi there");
    assert!(!out.executable);
}

#[test]
fn returns_not_found_for_missing_file() {
    let nar = dir_with_file("present.txt", b"x", false);
    let compressed = zstd_compress(&nar);

    let err =
        block_on(extract_file_from_nar_bytes(compressed, "missing.txt")).unwrap_err();
    assert!(matches!(err, ExtractError::NotFound));
}

#[test]
fn extracts_file_in_nested_directory() {
    let inner = Bytes::from_static(b"file text/tmp/out.txt");
    let events: TestNarEvents = vec![
        TestNarEvent::StartDirectory { name: Bytes::new() },
        TestNarEvent::StartDirectory { name: Bytes::from_static(b"nix-support") },
        TestNarEvent::File {
            name: Bytes::from_static(b"hydra-build-products"),
            executable: false,
            size: inner.len() as u64,
            reader: std::io::Cursor::new(inner.clone()),
        },
        TestNarEvent::EndDirectory,
        TestNarEvent::EndDirectory,
    ];
    let nar = write_nar(&events).to_vec();
    let compressed = zstd_compress(&nar);

    let out = block_on(extract_file_from_nar_bytes(
        compressed,
        "nix-support/hydra-build-products",
    ))
    .unwrap();
    assert_eq!(out.contents, &inner[..]);
}

#[test]
fn drains_non_matching_sibling_before_extracting_target() {
    // Sibling order: "a.txt" (drop), then "b.txt" (target).
    // If the non-matching file's reader isn't drained, parse_nar stalls.
    let drop_body = Bytes::from_static(b"do not read this");
    let target_body = Bytes::from_static(b"this is the one");
    let events: TestNarEvents = vec![
        TestNarEvent::StartDirectory { name: Bytes::new() },
        TestNarEvent::File {
            name: Bytes::from_static(b"a.txt"),
            executable: false,
            size: drop_body.len() as u64,
            reader: std::io::Cursor::new(drop_body),
        },
        TestNarEvent::File {
            name: Bytes::from_static(b"b.txt"),
            executable: false,
            size: target_body.len() as u64,
            reader: std::io::Cursor::new(target_body.clone()),
        },
        TestNarEvent::EndDirectory,
    ];
    let nar = write_nar(&events).to_vec();
    let compressed = zstd_compress(&nar);

    let out = block_on(extract_file_from_nar_bytes(compressed, "b.txt")).unwrap();
    assert_eq!(out.contents, &target_body[..]);
}
