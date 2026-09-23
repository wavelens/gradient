/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use bytes::Bytes;
use std::collections::BTreeMap;

pub struct NarFile {
    pub contents: Vec<u8>,
    pub executable: bool,
}

fn str(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u64).to_le_bytes());
    out.extend_from_slice(s);
    out.resize(out.len() + (8 - s.len() % 8) % 8, 0);
}

fn file(out: &mut Vec<u8>, f: &NarFile) {
    str(out, b"(");
    str(out, b"type");
    str(out, b"regular");
    if f.executable {
        str(out, b"executable");
        str(out, b"");
    }
    str(out, b"contents");
    str(out, &f.contents);
    str(out, b")");
}

fn dir(out: &mut Vec<u8>, files: &BTreeMap<String, NarFile>, prefix: &str) {
    str(out, b"(");
    str(out, b"type");
    str(out, b"directory");
    let mut names: Vec<&str> = files
        .keys()
        .filter_map(|k| k.strip_prefix(prefix))
        .map(|rest| rest.split('/').next().unwrap_or(rest))
        .collect();
    names.sort_unstable();
    names.dedup();
    for name in names {
        str(out, b"entry");
        str(out, b"(");
        str(out, b"name");
        str(out, name.as_bytes());
        str(out, b"node");
        let full = format!("{prefix}{name}");
        match files.get(&full) {
            Some(f) => file(out, f),
            None => dir(out, files, &format!("{full}/")),
        }
        str(out, b")");
    }
    str(out, b")");
}

pub fn encode(files: &BTreeMap<String, NarFile>) -> Bytes {
    let mut out = Vec::new();
    str(&mut out, b"nix-archive-1");
    match files.get("") {
        Some(root) => file(&mut out, root),
        None => dir(&mut out, files, ""),
    }
    Bytes::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nar_file(contents: &[u8], executable: bool) -> NarFile {
        NarFile {
            contents: contents.to_vec(),
            executable,
        }
    }

    #[test]
    fn encode_sorts_directory_entries() {
        let files = BTreeMap::from([
            ("b".to_owned(), nar_file(b"2", false)),
            ("a/x".to_owned(), nar_file(b"1", true)),
            ("a-b".to_owned(), nar_file(b"3", false)),
        ]);
        let text = String::from_utf8_lossy(&encode(&files)).into_owned();
        let at = |needle: &str| text.find(needle).expect(needle);
        assert!(at("a-b") > at("\x01\0\0\0\0\0\0\0a\0"));
        assert!(at("a-b") < at("\x01\0\0\0\0\0\0\0b\0"));
    }
}
