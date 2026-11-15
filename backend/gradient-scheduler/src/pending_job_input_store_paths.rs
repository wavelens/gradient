/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use gradient_entity::store_path::StorePath;
use gradient_entity::store_path_mapping::StorePathMapper;
use gradient_wire::worker_store_path_bloom_filter::decode_store_path_hash_into_two_u64_words;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingJobInputStorePath {
    pub store_path_bloom_filter_probe_words: Option<[u64; 2]>,
    pub nar_size: u64,
}

pub fn stored_store_path_to_bloom_filter_probe_words(
    mapper: Option<&StorePathMapper>,
    stored_path: &str,
) -> Option<[u64; 2]> {
    let path = StorePath::parse_stored(stored_path).ok()?;
    match mapper {
        Some(mapper) => {
            decode_store_path_hash_into_two_u64_words(mapper.outbound(&path).hash().as_str())
        }
        None => decode_store_path_hash_into_two_u64_words(path.hash()),
    }
}

pub fn build_pending_job_shared_inputs_from_stored_path_and_nar_size_pairs_with_optional_project_store_path_mapping<
    'a,
>(
    mapper: Option<&StorePathMapper>,
    paths: impl IntoIterator<Item = (&'a str, Option<u64>)>,
) -> Arc<[PendingJobInputStorePath]> {
    paths
        .into_iter()
        .map(|(path, nar_size)| PendingJobInputStorePath {
            store_path_bloom_filter_probe_words: stored_store_path_to_bloom_filter_probe_words(
                mapper, path,
            ),
            nar_size: nar_size.unwrap_or(0),
        })
        .collect()
}
