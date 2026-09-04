/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! On-disk NAR packing (`harmonia_file_nar::NarByteStream`), the path the worker
//! uses to upload every build output.
//!
//! The dumper splits on a 256 KiB threshold and takes a different route for
//! anything above it. That large-file route used to be an mmap of the store
//! file, which macOS validates the code signature of on every page fault: a
//! mach-o with an invalid signature SIGKILLs the packing process outright and
//! takes every in-progress build on that worker with it (#573). On Linux the
//! same mapping served uprobe breakpoint bytes instead of the file's real
//! contents (harmonia #1140). The tests below pack a real tree straight off disk
//! and compare it to `write_nar`, the reference encoder, so a large file's bytes
//! must survive the route the dumper picks for it.

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use bytes::Bytes;
use futures::StreamExt as _;
use harmonia_file_nar::NarByteStream;
use harmonia_file_nar::archive::test_data::{TestNarEvent, TestNarEvents};
use harmonia_file_nar::archive::write_nar;
use std::io::Write as _;
use std::path::Path;

/// The dumper's small-file threshold. Anything above it takes the other route.
const SMALL_FILE_THRESHOLD: usize = 256 * 1024;

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

/// Deterministic, incompressible-ish filler: a repeating byte pattern would
/// still compare equal after a whole chunk was dropped or duplicated.
fn filler(len: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

fn pack(path: &Path) -> Vec<u8> {
    block_on(async {
        let mut stream = NarByteStream::new(path.to_path_buf());
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.expect("NAR stream error"));
        }
        out
    })
}

fn expected_dir_with_file(name: &str, contents: &[u8], executable: bool) -> Vec<u8> {
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

fn write_tree(dir: &Path, name: &str, contents: &[u8]) {
    let mut f = std::fs::File::create(dir.join(name)).unwrap();
    f.write_all(contents).unwrap();
    f.sync_all().unwrap();
}

/// A file over the threshold must serialize to exactly its own bytes. This is
/// the regression guard for #573: the large-file route must read the file, not
/// map it.
#[test]
fn a_file_over_the_dumper_threshold_packs_its_real_bytes() {
    let contents = filler(SMALL_FILE_THRESHOLD * 4 + 7);
    let tmp = tempfile::tempdir().unwrap();
    write_tree(tmp.path(), "big", &contents);

    assert_eq!(
        pack(tmp.path()),
        expected_dir_with_file("big", &contents, false),
        "large-file NAR bytes diverged from the reference encoding"
    );
}

/// The threshold itself is a boundary the dumper branches on, so pin both sides
/// of it plus the exact boundary value.
#[test]
fn packing_is_byte_exact_across_the_threshold_boundary() {
    for size in [
        0,
        1,
        SMALL_FILE_THRESHOLD - 1,
        SMALL_FILE_THRESHOLD,
        SMALL_FILE_THRESHOLD + 1,
    ] {
        let contents = filler(size);
        let tmp = tempfile::tempdir().unwrap();
        write_tree(tmp.path(), "f", &contents);

        assert_eq!(
            pack(tmp.path()),
            expected_dir_with_file("f", &contents, false),
            "NAR bytes diverged at size {size}"
        );
    }
}

/// A multi-gigabyte store path is streamed, not buffered: chunk boundaries must
/// not reorder or drop content across several large files in one archive.
#[test]
fn several_large_files_pack_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let a = filler(SMALL_FILE_THRESHOLD * 2);
    let b = filler(SMALL_FILE_THRESHOLD * 3 + 11);
    write_tree(tmp.path(), "a", &a);
    write_tree(tmp.path(), "b", &b);

    let events: TestNarEvents = vec![
        TestNarEvent::StartDirectory { name: Bytes::new() },
        TestNarEvent::File {
            name: Bytes::from_static(b"a"),
            executable: false,
            size: a.len() as u64,
            reader: std::io::Cursor::new(Bytes::from(a.clone())),
        },
        TestNarEvent::File {
            name: Bytes::from_static(b"b"),
            executable: false,
            size: b.len() as u64,
            reader: std::io::Cursor::new(Bytes::from(b.clone())),
        },
        TestNarEvent::EndDirectory,
    ];

    assert_eq!(pack(tmp.path()), write_nar(&events).to_vec());
}
