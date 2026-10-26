/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result, bail};
use gradient_types::ids::CacheId;
use gradient_wire::constants::NAR_ZSTD_LEVEL;
use rkyv::rancor::Error as RkyvError;
use rkyv::{Archive, Deserialize, Serialize};
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashSet};
use std::io::Read as _;
use tracing::warn;
use uuid::Uuid;

pub const ATTR_PLACEHOLDER: u8 = 0;
pub const ATTR_FULL_ATTRS: u8 = 1;
pub const ATTR_MISSING: u8 = 3;
pub const ATTR_FAILED: u8 = 5;

pub const SEGMENT_MAX_BYTES: usize = 8 * 1024 * 1024;

const SEGMENT_FORMAT: u8 = 1;

const DECOMPRESSED_MAX_BYTES: usize = 16 * SEGMENT_MAX_BYTES;

#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
pub struct EvalCacheRow {
    pub path: Vec<String>,
    pub kind: u8,
    pub value: Option<String>,
    pub context: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Merge {
    Keep,
    Take,
}

pub fn eval_cache_segment_key(cache: CacheId, fingerprint: &str, segment: Uuid) -> String {
    format!(
        "{}/{segment}",
        eval_cache_segment_prefix(cache, fingerprint)
    )
}

pub fn eval_cache_segment_prefix(cache: CacheId, fingerprint: &str) -> String {
    format!("eval-cache-segments/{cache}/{fingerprint}")
}

fn rank(kind: u8) -> u8 {
    match kind {
        ATTR_PLACEHOLDER => 0,
        ATTR_FAILED => 1,
        _ => 2,
    }
}

pub fn merge(existing: &EvalCacheRow, incoming: &EvalCacheRow) -> Merge {
    if rank(incoming.kind) > rank(existing.kind) {
        Merge::Take
    } else {
        Merge::Keep
    }
}

pub fn encode(rows: &[EvalCacheRow]) -> Result<Vec<u8>> {
    compress(&archive(rows)?)
}

fn archive(rows: &[EvalCacheRow]) -> Result<rkyv::util::AlignedVec> {
    rkyv::to_bytes::<RkyvError>(&rows.to_vec()).context("archive eval cache rows")
}

fn compress(archive: &[u8]) -> Result<Vec<u8>> {
    let compressed =
        zstd::bulk::compress(archive, NAR_ZSTD_LEVEL).context("compress eval cache segment")?;

    let mut bytes = Vec::with_capacity(compressed.len() + 1);
    bytes.push(SEGMENT_FORMAT);
    bytes.extend_from_slice(&compressed);
    Ok(bytes)
}

pub fn encode_bounded(rows: &[EvalCacheRow]) -> Result<Vec<Vec<u8>>> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut chunk_size = 0;
    for (index, row) in rows.iter().enumerate() {
        let size = estimated_size(row);
        if index > start && chunk_size + size > SEGMENT_MAX_BYTES {
            encode_within_limit(&rows[start..index], &mut segments)?;
            start = index;
            chunk_size = 0;
        }

        chunk_size += size;
    }

    if start < rows.len() {
        encode_within_limit(&rows[start..], &mut segments)?;
    }

    Ok(segments)
}

fn estimated_size(row: &EvalCacheRow) -> usize {
    let text = |part: &Option<String>| part.as_ref().map_or(0, String::len) + 16;
    32 + row.path.iter().map(|part| part.len() + 16).sum::<usize>()
        + text(&row.value)
        + text(&row.context)
}

fn encode_within_limit(rows: &[EvalCacheRow], segments: &mut Vec<Vec<u8>>) -> Result<()> {
    let archive = archive(rows)?;
    if archive.len() <= DECOMPRESSED_MAX_BYTES {
        let bytes = compress(&archive)?;
        if bytes.len() <= SEGMENT_MAX_BYTES {
            segments.push(bytes);
            return Ok(());
        }
    }

    if let [row] = rows {
        warn!(path = ?row.path, "eval cache row is larger than a segment; keeping it local");
        return Ok(());
    }

    let (first, second) = rows.split_at(rows.len() / 2);
    encode_within_limit(first, segments)?;
    encode_within_limit(second, segments)
}

// A FullAttrs row lists its children as `<byte length>:<name>`, since a Nix attribute name may hold any character.
pub fn child_listing(mut names: Vec<String>) -> String {
    names.sort();
    names
        .iter()
        .map(|name| format!("{}:{name}", name.len()))
        .collect()
}

pub fn listed_children(listing: Option<&str>) -> Result<Vec<String>> {
    let mut rest = listing.unwrap_or_default();
    let mut names = Vec::new();
    while !rest.is_empty() {
        let (length, tail) = rest
            .split_once(':')
            .context("eval cache child listing lacks a length")?;
        let length: usize = length
            .parse()
            .context("eval cache child listing has a bad length")?;
        let (name, tail) = tail
            .split_at_checked(length)
            .context("eval cache child listing is truncated")?;
        names.push(name.to_owned());
        rest = tail;
    }

    Ok(names)
}

pub fn decode(bytes: &[u8]) -> Result<Vec<EvalCacheRow>> {
    let Some((&format, compressed)) = bytes.split_first() else {
        bail!("empty eval cache segment");
    };

    if format != SEGMENT_FORMAT {
        bail!("unknown eval cache segment format {format}");
    }

    let mut archive = Vec::new();
    zstd::stream::read::Decoder::new(compressed)
        .context("open eval cache segment")?
        .take(DECOMPRESSED_MAX_BYTES as u64 + 1)
        .read_to_end(&mut archive)
        .context("decompress eval cache segment")?;
    if archive.len() > DECOMPRESSED_MAX_BYTES {
        bail!("eval cache segment inflates past {DECOMPRESSED_MAX_BYTES} bytes");
    }

    rkyv::from_bytes::<Vec<EvalCacheRow>, RkyvError>(&archive).context("read eval cache segment")
}

pub fn merge_segments(segments: impl IntoIterator<Item = Vec<EvalCacheRow>>) -> Vec<EvalCacheRow> {
    let mut rows: BTreeMap<Vec<String>, EvalCacheRow> = BTreeMap::new();
    for incoming in segments.into_iter().flatten() {
        match rows.entry(incoming.path.clone()) {
            Entry::Vacant(slot) => {
                slot.insert(incoming);
            }
            Entry::Occupied(mut slot) => merge_into(slot.get_mut(), incoming),
        }
    }

    let sealed: HashSet<&[String]> = rows
        .values()
        .filter(|row| row.kind == ATTR_FULL_ATTRS)
        .map(|row| row.path.as_slice())
        .collect();
    let phantoms: Vec<Vec<String>> = rows
        .values()
        .filter(|row| {
            row.kind == ATTR_MISSING
                && row
                    .path
                    .split_last()
                    .is_some_and(|(_, parent)| sealed.contains(parent))
        })
        .map(|row| row.path.clone())
        .collect();
    for path in &phantoms {
        rows.remove(path);
    }

    rows.into_values().collect()
}

fn merge_into(existing: &mut EvalCacheRow, incoming: EvalCacheRow) {
    if existing.kind == ATTR_FULL_ATTRS
        && incoming.kind == ATTR_FULL_ATTRS
        && existing.value != incoming.value
    {
        warn!(path = ?existing.path, "eval cache segments disagree on an attribute set");
    }

    if merge(existing, &incoming) == Merge::Take {
        *existing = incoming;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &[&str], kind: u8, value: &str) -> EvalCacheRow {
        EvalCacheRow {
            path: path.iter().map(|part| (*part).to_owned()).collect(),
            kind,
            value: Some(value.to_owned()),
            context: None,
        }
    }

    #[test]
    fn a_value_replaces_a_placeholder_and_a_failure() {
        assert_eq!(
            merge(&row(&["a"], ATTR_PLACEHOLDER, ""), &row(&["a"], 2, "x")),
            Merge::Take
        );
        assert_eq!(
            merge(&row(&["a"], ATTR_FAILED, ""), &row(&["a"], 2, "x")),
            Merge::Take
        );
        assert_eq!(
            merge(&row(&["a"], 2, "x"), &row(&["a"], ATTR_FAILED, "")),
            Merge::Keep
        );
        assert_eq!(
            merge(&row(&["a"], 2, "x"), &row(&["a"], ATTR_PLACEHOLDER, "")),
            Merge::Keep
        );
    }

    #[test]
    fn equal_rank_keeps_the_existing_row() {
        assert_eq!(
            merge(&row(&["a"], 2, "x"), &row(&["a"], 2, "y")),
            Merge::Keep
        );
    }

    #[test]
    fn merging_a_segment_twice_changes_nothing() {
        let s = vec![row(&[], ATTR_FULL_ATTRS, "a"), row(&["a"], 2, "x")];
        assert_eq!(merge_segments([s.clone()]), merge_segments([s.clone(), s]));
    }

    #[test]
    fn round_trip_keeps_every_field() {
        let rows = vec![EvalCacheRow {
            path: vec!["packages".into(), "x86_64-linux".into()],
            kind: 7,
            value: Some("a\tb".into()),
            context: Some("/nix/store/x.drv".into()),
        }];
        assert_eq!(decode(&encode(&rows).unwrap()).unwrap(), rows);
    }

    #[test]
    fn decode_rejects_unknown_format_and_truncation() {
        let bytes = encode(&[row(&["a"], 2, "x")]).unwrap();
        let mut wrong = bytes.clone();
        wrong[0] = 0xff;
        assert!(decode(&wrong).is_err());
        assert!(decode(&bytes[..bytes.len() / 2]).is_err());
    }

    #[test]
    fn merge_drops_missing_rows_under_a_full_attrs_parent() {
        let before_seal = vec![
            row(&[], ATTR_PLACEHOLDER, ""),
            row(&["type"], ATTR_MISSING, ""),
            row(&["b"], ATTR_PLACEHOLDER, ""),
            row(&["b", "type"], ATTR_MISSING, ""),
        ];
        let sealed = vec![
            row(&[], ATTR_FULL_ATTRS, &child_listing(vec!["b".into()])),
            row(&["b"], ATTR_PLACEHOLDER, ""),
        ];

        let paths: Vec<Vec<String>> = merge_segments([before_seal, sealed])
            .into_iter()
            .map(|row| row.path)
            .collect();

        assert_eq!(
            paths,
            vec![
                vec![],
                vec!["b".to_owned()],
                vec!["b".to_owned(), "type".to_owned()]
            ]
        );
    }

    #[test]
    fn a_child_listing_keeps_any_attribute_name() {
        let mut names = vec![
            "b\tc".to_owned(),
            String::new(),
            "a:1".to_owned(),
            "ä".to_owned(),
        ];
        let listing = child_listing(names.clone());

        names.sort();
        assert_eq!(listed_children(Some(&listing)).unwrap(), names);
        assert!(listed_children(None).unwrap().is_empty());
        assert!(listed_children(Some("5:ab")).is_err());
    }

    #[test]
    fn encode_bounded_splits_at_the_segment_size() {
        let value = "x".repeat(1024 * 1024);
        let rows: Vec<EvalCacheRow> = (0..10)
            .map(|child| row(&[&format!("c{child}")], 2, &value))
            .collect();

        let segments = encode_bounded(&rows).unwrap();

        assert_eq!(segments.len(), 2);
        assert!(segments.iter().all(|s| s.len() <= SEGMENT_MAX_BYTES));
        let decoded: Vec<EvalCacheRow> = segments.iter().flat_map(|s| decode(s).unwrap()).collect();
        assert_eq!(decoded, rows);
    }

    #[test]
    fn decode_rejects_a_segment_that_inflates_past_the_cap() {
        use std::io::Write as _;

        let mut encoder = zstd::stream::write::Encoder::new(vec![SEGMENT_FORMAT], 1).unwrap();
        let zeros = vec![0u8; 1024 * 1024];
        for _ in 0..=DECOMPRESSED_MAX_BYTES / zeros.len() {
            encoder.write_all(&zeros).unwrap();
        }
        let bomb = encoder.finish().unwrap();

        let error = decode(&bomb).unwrap_err().to_string();
        assert!(error.contains("inflates past"), "{error}");
    }
}
