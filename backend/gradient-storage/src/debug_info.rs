/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! DWARF build-id discovery inside a NAR.
//!
//! nixpkgs' `separateDebugInfo` writes debug files to
//! `lib/debug/.build-id/<xx>/<yyyy>.debug`, where `<xx>` is the first byte of the
//! ELF build id and `<yyyy>` the remaining 19. Walking those entries is what nix
//! does when a binary cache is created with `index-debug-info=true`, and it is
//! what lets a debuginfod client resolve a build id to a NAR member.

use futures::StreamExt as _;
use harmonia_file_nar::{NarEvent, parse_nar};
use std::io;

/// Directory that holds the build-id tree inside a `separateDebugInfo` output.
const BUILD_ID_DIR: [&str; 3] = ["lib", "debug", ".build-id"];

/// One `lib/debug/.build-id` member found in a NAR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildIdEntry {
    /// 40-char lowercase hex build id, without the `.debug` suffix.
    pub build_id: String,
    /// NAR-relative path of the debug file.
    pub member: String,
}

/// Streams the (already decompressed) NAR and returns every well-formed
/// `lib/debug/.build-id/<xx>/<yyyy>.debug` member. File bodies are drained, not
/// buffered, so a multi-gigabyte debug output costs bandwidth but not memory.
pub async fn scan_build_ids<R>(reader: R) -> io::Result<Vec<BuildIdEntry>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut stream = parse_nar(reader);
    let mut dirs: Vec<String> = Vec::new();
    let mut found = Vec::new();

    while let Some(event) = stream.next().await {
        match event? {
            NarEvent::StartDirectory { name } => {
                dirs.push(entry_name(&name)?);
            }
            NarEvent::EndDirectory => {
                dirs.pop();
            }
            NarEvent::File {
                name, mut reader, ..
            } => {
                tokio::io::copy(&mut reader, &mut tokio::io::sink()).await?;
                let name = entry_name(&name)?;
                if let Some(entry) = build_id_entry(&dirs, &name) {
                    found.push(entry);
                }
            }
            NarEvent::Symlink { .. } => {}
        }
    }

    Ok(found)
}

fn entry_name(name: &bytes::Bytes) -> io::Result<String> {
    std::str::from_utf8(name)
        .map(str::to_owned)
        .map_err(|e| io::Error::other(format!("non-UTF-8 NAR entry name: {e}")))
}

/// Matches `<root>/lib/debug/.build-id/<xx>/<yyyy>.debug` and joins the two hex
/// halves into the build id; `dirs` starts with the root's empty name.
fn build_id_entry(dirs: &[String], file_name: &str) -> Option<BuildIdEntry> {
    let (prefix, parents) = dirs.split_first()?;
    if !prefix.is_empty() || parents.len() != BUILD_ID_DIR.len() + 1 {
        return None;
    }
    let (bucket, tree) = parents.split_last()?;
    if tree != BUILD_ID_DIR || !is_hex(bucket, 2) {
        return None;
    }

    let rest = file_name.strip_suffix(".debug")?;
    if !is_hex(rest, 38) {
        return None;
    }

    Some(BuildIdEntry {
        build_id: format!("{bucket}{rest}"),
        member: format!("{}/{bucket}/{file_name}", BUILD_ID_DIR.join("/")),
    })
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::TryStreamExt as _;
    use harmonia_file_nar::NarByteStream;
    use std::path::Path;

    const BUCKET: &str = "7d";
    const REST: &str = "beaca53fbc9a489b633871093c37dae3857a37";

    async fn nar_of(dir: &Path) -> Vec<u8> {
        let chunks: Vec<bytes::Bytes> = NarByteStream::new(dir.to_path_buf())
            .try_collect()
            .await
            .expect("dump NAR");
        chunks.into_iter().flatten().collect()
    }

    fn write_debug_file(root: &Path, bucket: &str, file: &str) {
        let dir = root.join("lib/debug/.build-id").join(bucket);
        std::fs::create_dir_all(&dir).expect("create build-id dir");
        std::fs::write(dir.join(file), b"\x7fELF").expect("write debug file");
    }

    #[tokio::test]
    async fn finds_separate_debug_info_members() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        write_debug_file(tmp.path(), BUCKET, &format!("{REST}.debug"));
        std::fs::create_dir_all(tmp.path().join("bin")).expect("create bin");
        std::fs::write(tmp.path().join("bin/hello"), b"hi").expect("write hello");

        let found = scan_build_ids(std::io::Cursor::new(nar_of(tmp.path()).await))
            .await
            .expect("scan");

        assert_eq!(
            found,
            vec![BuildIdEntry {
                build_id: format!("{BUCKET}{REST}"),
                member: format!("lib/debug/.build-id/{BUCKET}/{REST}.debug"),
            }]
        );
    }

    #[tokio::test]
    async fn ignores_malformed_and_misplaced_entries() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        write_debug_file(tmp.path(), BUCKET, "notlongenough.debug");
        write_debug_file(tmp.path(), BUCKET, &format!("{REST}.txt"));
        write_debug_file(tmp.path(), "zz", &format!("{REST}.debug"));

        let nested = tmp.path().join("share/lib/debug/.build-id").join(BUCKET);
        std::fs::create_dir_all(&nested).expect("create nested");
        std::fs::write(nested.join(format!("{REST}.debug")), b"x").expect("write nested");

        let found = scan_build_ids(std::io::Cursor::new(nar_of(tmp.path()).await))
            .await
            .expect("scan");

        assert!(found.is_empty(), "unexpected matches: {found:?}");
    }

    #[tokio::test]
    async fn a_nar_without_debug_info_yields_nothing() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("bin")).expect("create bin");
        std::fs::write(tmp.path().join("bin/hello"), b"hi").expect("write hello");

        let found = scan_build_ids(std::io::Cursor::new(nar_of(tmp.path()).await))
            .await
            .expect("scan");

        assert!(found.is_empty());
    }
}
