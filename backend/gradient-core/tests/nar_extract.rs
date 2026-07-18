/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for `gradient_storage::nar_extract`.
//!
//! Async assertions use sync `#[test]` + `tokio::runtime::Builder::block_on`.

use bytes::Bytes;
use futures::StreamExt as _;
use gradient_storage::nar_extract::{
    ExtractError, Extracted, extract_path_from_nar_bytes, extract_path_from_reader,
    nar_reader_from_stream,
};
use harmonia_file_nar::archive::test_data::{TestNarEvent, TestNarEvents};
use harmonia_file_nar::archive::write_nar;
use std::collections::BTreeMap;
use std::io::Read as _;

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

fn unwrap_file(out: Extracted) -> (Vec<u8>, bool, u64) {
    match out {
        Extracted::File {
            contents,
            executable,
            size,
        } => (contents, executable, size),
        Extracted::Directory { .. } => panic!("expected File, got Directory"),
    }
}

fn unwrap_dir(out: Extracted) -> Vec<u8> {
    match out {
        Extracted::Directory { tar_zst } => tar_zst,
        Extracted::File { .. } => panic!("expected Directory, got File"),
    }
}

type FileMap = BTreeMap<String, (u32, Vec<u8>)>;

/// Decompress a `tar.zst` archive and return a map of path → (mode-bits, body)
/// for regular files, plus a sorted list of all entry paths (including dirs
/// and symlinks) so tests can assert on structure.
fn read_tar_zst(tar_zst: &[u8]) -> (FileMap, Vec<String>) {
    let tar_bytes = zstd::decode_all(std::io::Cursor::new(tar_zst)).unwrap();
    let mut archive = tar::Archive::new(std::io::Cursor::new(tar_bytes));
    let mut files: BTreeMap<String, (u32, Vec<u8>)> = BTreeMap::new();
    let mut paths: Vec<String> = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        paths.push(path.clone());
        if entry.header().entry_type() == tar::EntryType::Regular {
            let mode = entry.header().mode().unwrap();
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).unwrap();
            files.insert(path, (mode, buf));
        }
    }
    paths.sort();
    (files, paths)
}

#[test]
fn extracts_file_at_relative_path() {
    let nar = dir_with_file("hello.txt", b"hi there", false);
    let compressed = zstd_compress(&nar);

    let out = block_on(extract_path_from_nar_bytes(compressed, "hello.txt")).unwrap();
    let (contents, executable, _) = unwrap_file(out);
    assert_eq!(contents, b"hi there");
    assert!(!executable);
}

/// The streaming bridge used by the serve/browse endpoints must decode and
/// extract identically to the buffered path, even when the compressed input is
/// delivered as many small chunks that split the zstd frame across boundaries.
#[test]
fn streaming_reader_extracts_same_file_as_buffered() {
    let nar = dir_with_file("hello.txt", b"streamed hi", false);
    let compressed = zstd_compress(&nar);

    let buffered = block_on(extract_path_from_nar_bytes(compressed.clone(), "hello.txt")).unwrap();
    let (buffered_contents, _, _) = unwrap_file(buffered);

    let chunks: Vec<anyhow::Result<Bytes>> = compressed
        .chunks(48)
        .map(|c| Ok(Bytes::copy_from_slice(c)))
        .collect();
    let stream = futures::stream::iter(chunks).boxed();
    let streamed = block_on(extract_path_from_reader(
        nar_reader_from_stream(stream),
        "hello.txt",
    ))
    .unwrap();
    let (streamed_contents, _, _) = unwrap_file(streamed);

    assert_eq!(streamed_contents, b"streamed hi");
    assert_eq!(streamed_contents, buffered_contents);
}

#[test]
fn returns_not_found_for_missing_path() {
    let nar = dir_with_file("present.txt", b"x", false);
    let compressed = zstd_compress(&nar);

    let err = block_on(extract_path_from_nar_bytes(compressed, "missing.txt")).unwrap_err();
    assert!(matches!(err, ExtractError::NotFound));
}

#[test]
fn extracts_file_in_nested_directory() {
    let inner = Bytes::from_static(b"file text/tmp/out.txt");
    let events: TestNarEvents = vec![
        TestNarEvent::StartDirectory { name: Bytes::new() },
        TestNarEvent::StartDirectory {
            name: Bytes::from_static(b"nix-support"),
        },
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

    let out = block_on(extract_path_from_nar_bytes(
        compressed,
        "nix-support/hydra-build-products",
    ))
    .unwrap();
    let (contents, _, _) = unwrap_file(out);
    assert_eq!(contents, &inner[..]);
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

    let out = block_on(extract_path_from_nar_bytes(compressed, "b.txt")).unwrap();
    let (contents, _, _) = unwrap_file(out);
    assert_eq!(contents, &target_body[..]);
}

/// Regression for "fails if build output is a folder": when the requested
/// relative path is a directory rather than a file, return a `tar.zst` of
/// the subtree instead of erroring.
#[test]
fn extracts_directory_as_tar_zst() {
    let bin_body = Bytes::from_static(b"#!/bin/sh\necho hi\n");
    let conf_body = Bytes::from_static(b"x=1\n");
    let events: TestNarEvents = vec![
        TestNarEvent::StartDirectory { name: Bytes::new() },
        TestNarEvent::StartDirectory {
            name: Bytes::from_static(b"store"),
        },
        TestNarEvent::StartDirectory {
            name: Bytes::from_static(b"bin"),
        },
        TestNarEvent::File {
            name: Bytes::from_static(b"hello"),
            executable: true,
            size: bin_body.len() as u64,
            reader: std::io::Cursor::new(bin_body.clone()),
        },
        TestNarEvent::EndDirectory, // bin
        TestNarEvent::File {
            name: Bytes::from_static(b"config"),
            executable: false,
            size: conf_body.len() as u64,
            reader: std::io::Cursor::new(conf_body.clone()),
        },
        TestNarEvent::EndDirectory, // store
        TestNarEvent::EndDirectory, // root
    ];
    let nar = write_nar(&events).to_vec();
    let compressed = zstd_compress(&nar);

    let out = block_on(extract_path_from_nar_bytes(compressed, "store")).unwrap();
    let tar_zst = unwrap_dir(out);
    let (files, paths) = read_tar_zst(&tar_zst);

    // Tar should contain the matched dir as the root, then nested entries
    // - every path is rooted at "store/" so extraction recreates that name.
    assert!(
        paths.iter().any(|p| p == "store/"),
        "missing root dir entry: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p == "store/bin/"),
        "missing nested dir entry: {paths:?}"
    );
    assert!(paths.iter().any(|p| p == "store/bin/hello"));
    assert!(paths.iter().any(|p| p == "store/config"));

    let (mode_hello, body_hello) = files.get("store/bin/hello").unwrap();
    assert_eq!(body_hello, &bin_body[..]);
    assert_eq!(*mode_hello & 0o111, 0o111, "executable bit should be set");

    let (mode_conf, body_conf) = files.get("store/config").unwrap();
    assert_eq!(body_conf, &conf_body[..]);
    assert_eq!(*mode_conf & 0o111, 0, "non-executable bit should be unset");
}

/// Symlinks inside the matched subtree must be preserved in the tarball.
#[test]
fn directory_tarball_preserves_symlinks() {
    let body = Bytes::from_static(b"target");
    let events: TestNarEvents = vec![
        TestNarEvent::StartDirectory { name: Bytes::new() },
        TestNarEvent::StartDirectory {
            name: Bytes::from_static(b"out"),
        },
        // NAR directory entries must be name-sorted (`link` < `real`), matching
        // Nix's parser, which rejects out-of-order entries.
        TestNarEvent::Symlink {
            name: Bytes::from_static(b"link"),
            target: Bytes::from_static(b"real"),
        },
        TestNarEvent::File {
            name: Bytes::from_static(b"real"),
            executable: false,
            size: body.len() as u64,
            reader: std::io::Cursor::new(body.clone()),
        },
        TestNarEvent::EndDirectory,
        TestNarEvent::EndDirectory,
    ];
    let nar = write_nar(&events).to_vec();
    let compressed = zstd_compress(&nar);

    let out = block_on(extract_path_from_nar_bytes(compressed, "out")).unwrap();
    let tar_zst = unwrap_dir(out);

    let tar_bytes = zstd::decode_all(std::io::Cursor::new(&tar_zst)).unwrap();
    let mut archive = tar::Archive::new(std::io::Cursor::new(tar_bytes));
    let mut found_link_target: Option<String> = None;
    for entry in archive.entries().unwrap() {
        let entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        if path == "out/link" {
            assert_eq!(entry.header().entry_type(), tar::EntryType::Symlink);
            let link = entry.link_name().unwrap().expect("link target");
            found_link_target = Some(link.to_string_lossy().into_owned());
        }
    }
    assert_eq!(found_link_target.as_deref(), Some("real"));
}

/// When the requested path is the NAR's root directory itself (target of a
/// single empty component), we still return a tarball rather than erroring -
/// this happens when a build product's path equals the output store path.
#[test]
fn directory_match_at_root_via_basename() {
    // Build a NAR whose root *is* the matched directory by using a top-level
    // file: target = "myout" hits via a directory called "myout" at depth 1.
    let body = Bytes::from_static(b"hello");
    let events: TestNarEvents = vec![
        TestNarEvent::StartDirectory { name: Bytes::new() },
        TestNarEvent::StartDirectory {
            name: Bytes::from_static(b"myout"),
        },
        TestNarEvent::File {
            name: Bytes::from_static(b"only.txt"),
            executable: false,
            size: body.len() as u64,
            reader: std::io::Cursor::new(body.clone()),
        },
        TestNarEvent::EndDirectory,
        TestNarEvent::EndDirectory,
    ];
    let nar = write_nar(&events).to_vec();
    let compressed = zstd_compress(&nar);

    let out = block_on(extract_path_from_nar_bytes(compressed, "myout")).unwrap();
    let tar_zst = unwrap_dir(out);
    let (files, _paths) = read_tar_zst(&tar_zst);
    assert_eq!(files.get("myout/only.txt").unwrap().1, &body[..]);
}
