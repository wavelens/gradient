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
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use tracing::warn;
use uuid::Uuid;

pub const ATTR_PLACEHOLDER: u8 = 0;
pub const ATTR_FULL_ATTRS: u8 = 1;
pub const ATTR_FAILED: u8 = 5;

pub const SEGMENT_MAX_BYTES: usize = 8 * 1024 * 1024;

const SEGMENT_FORMAT: u8 = 1;

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

pub(crate) fn eval_cache_segment_prefix(cache: CacheId, fingerprint: &str) -> String {
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
    let archive = rkyv::to_bytes::<RkyvError>(&rows.to_vec()).context("archive eval cache rows")?;
    let compressed =
        zstd::bulk::compress(&archive, NAR_ZSTD_LEVEL).context("compress eval cache segment")?;

    let mut bytes = Vec::with_capacity(compressed.len() + 1);
    bytes.push(SEGMENT_FORMAT);
    bytes.extend_from_slice(&compressed);
    Ok(bytes)
}

pub fn decode(bytes: &[u8]) -> Result<Vec<EvalCacheRow>> {
    let Some((&format, compressed)) = bytes.split_first() else {
        bail!("empty eval cache segment");
    };

    if format != SEGMENT_FORMAT {
        bail!("unknown eval cache segment format {format}");
    }

    let archive = zstd::stream::decode_all(compressed).context("decompress eval cache segment")?;
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
}
