/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

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

const SMALL_FILE_THRESHOLD: usize = 256 * 1024;

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

/// A repeating byte pattern would still compare equal after a whole chunk was dropped or
/// duplicated.
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

/// Regression guard for #573. The large-file route must read the file, not map it. macOS was
/// killing the packer on page faults of mapped mach-o files with an invalid signature.
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
