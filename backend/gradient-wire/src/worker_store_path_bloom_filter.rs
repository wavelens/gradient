/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use bytes::{Bytes, BytesMut};
use gradient_util::nix_hash::nix32_decode;

use crate::messages::ClientMessage;

pub const FILTER_BITS_PER_STORE_PATH: u64 = 20;
pub const MIN_STORE_PATH_COUNT_FOR_WORKER_STORE_PATH_BLOOM_FILTER: usize = 1024;
pub const BLOOM_FILTER_BIT_POSITIONS_CHECKED_PER_ENTRY: u8 = 14;
pub const MAX_BLOOM_FILTER_BYTES_PER_WIRE_CHUNK: usize = 1024 * 1024;
pub const MAX_WORKER_STORE_PATH_BLOOM_FILTER_BIT_COUNT: u64 =
    16_000_000 * FILTER_BITS_PER_STORE_PATH;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerStorePathBloomFilter {
    bit_count: u64,
    probe_count: u8,
    bits: BytesMut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BloomFilterBitArrayChunk {
    pub bit_count: u64,
    pub probe_count: u8,
    pub offset: u64,
    pub bits: Bytes,
    pub is_final: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BloomFilterChunkRejectionReason {
    #[error("chunk offset does not start at the end of the previous chunk")]
    ChunkByteOffsetDoesNotContinuePreviousChunk,
    #[error("a chunk with a different bit count or probe count than the first chunk")]
    ChunkBitCountOrProbeCountDiffersFromFirstChunk,
    #[error("received byte count does not match specified bit count")]
    ByteCountDoesNotMatchBitCount,
    #[error("first chunk with zero bits or zero probes")]
    FirstChunkHasZeroBitCountOrProbeCount,
    #[error("leading chunk declared more bits than the maximum filter size")]
    FirstChunkFilterBitCountTooLarge,
}

#[derive(Debug, Default)]
pub struct WorkerStorePathBloomFilterChunkCollector {
    pending: Option<BloomFilterChunksReceivedSoFar>,
}

#[derive(Debug)]
struct BloomFilterChunksReceivedSoFar {
    bit_count: u64,
    probe_count: u8,
    bits: BytesMut,
}

// Store path hashes already give 160 uniformly distributed bits. Their first two words drive the
// probes. The probe calculation can skip another hash function.
pub fn decode_store_path_hash_into_two_u64_words(path_hash: &str) -> Option<[u64; 2]> {
    let bytes = nix32_decode(path_hash).filter(|bytes| bytes.len() == 20)?;
    let word = |start: usize| {
        bytes[start..start + 8]
            .try_into()
            .ok()
            .map(u64::from_le_bytes)
    };
    Some([word(0)?, word(8)?])
}

impl WorkerStorePathBloomFilter {
    pub fn new_for_store_path_count(path_count: usize) -> Self {
        let bit_count = (path_count.max(MIN_STORE_PATH_COUNT_FOR_WORKER_STORE_PATH_BLOOM_FILTER)
            as u64)
            * FILTER_BITS_PER_STORE_PATH;
        let byte_count = usize::try_from(bit_count.div_ceil(8)).unwrap_or(usize::MAX);
        Self {
            bit_count,
            probe_count: BLOOM_FILTER_BIT_POSITIONS_CHECKED_PER_ENTRY,
            bits: BytesMut::zeroed(byte_count),
        }
    }

    pub fn from_store_path_hash_parts(path_hashes: &[impl AsRef<str>]) -> Self {
        let mut filter = Self::new_for_store_path_count(path_hashes.len());
        path_hashes
            .iter()
            .filter_map(|hash| decode_store_path_hash_into_two_u64_words(hash.as_ref()))
            .for_each(|words| filter.insert_probe_words_into_filter(words));
        filter
    }

    pub fn insert_probe_words_into_filter(&mut self, probe_words: [u64; 2]) {
        for position in self.entry_probe_bit_positions(probe_words) {
            let (byte, mask) = bit_position_to_byte_index_and_mask(position);
            if let Some(bits) = self.bits.get_mut(byte) {
                *bits |= mask;
            }
        }
    }

    pub fn probably_contains_probe_words(&self, probe_words: [u64; 2]) -> bool {
        self.entry_probe_bit_positions(probe_words).all(|position| {
            let (byte, mask) = bit_position_to_byte_index_and_mask(position);
            self.bits.get(byte).is_some_and(|bits| bits & mask != 0)
        })
    }

    pub fn into_wire_chunks_of_at_most_max_chunk_size_with_last_final(
        self,
    ) -> Vec<BloomFilterBitArrayChunk> {
        let bits = self.bits.freeze();
        (0..bits.len())
            .step_by(MAX_BLOOM_FILTER_BYTES_PER_WIRE_CHUNK)
            .map(|start| {
                let end = (start + MAX_BLOOM_FILTER_BYTES_PER_WIRE_CHUNK).min(bits.len());
                BloomFilterBitArrayChunk {
                    bit_count: self.bit_count,
                    probe_count: self.probe_count,
                    offset: start as u64,
                    bits: bits.slice(start..end),
                    is_final: end == bits.len(),
                }
            })
            .collect()
    }

    fn entry_probe_bit_positions(&self, [h1, h2]: [u64; 2]) -> impl Iterator<Item = u64> + use<> {
        let modulus = self.bit_count;
        std::iter::successors(
            Some((1, h1 % modulus, h2 % modulus)),
            move |&(probe_index, position, step)| {
                Some((
                    probe_index + 1,
                    add_two_numbers_below_modulus_modulo_modulus_without_overflow(
                        position, step, modulus,
                    ),
                    add_two_numbers_below_modulus_modulo_modulus_without_overflow(
                        step,
                        probe_index % modulus,
                        modulus,
                    ),
                ))
            },
        )
        .map(|(_, position, _)| position)
        .take(usize::from(self.probe_count))
    }
}

impl WorkerStorePathBloomFilterChunkCollector {
    pub fn add_chunk_and_return_complete_filter_if_final_chunk(
        &mut self,
        chunk: BloomFilterBitArrayChunk,
    ) -> Result<Option<WorkerStorePathBloomFilter>, BloomFilterChunkRejectionReason> {
        let mut pending = match self.pending.take() {
            Some(pending) if chunk.offset != 0 => pending,
            _ => BloomFilterChunksReceivedSoFar::start(&chunk)?,
        };
        pending.append(&chunk)?;
        if !chunk.is_final {
            self.pending = Some(pending);
            return Ok(None);
        }
        pending.finish().map(Some)
    }
}

impl BloomFilterChunksReceivedSoFar {
    fn start(chunk: &BloomFilterBitArrayChunk) -> Result<Self, BloomFilterChunkRejectionReason> {
        if chunk.offset != 0 {
            return Err(
                BloomFilterChunkRejectionReason::ChunkByteOffsetDoesNotContinuePreviousChunk,
            );
        }
        if chunk.bit_count == 0 || chunk.probe_count == 0 {
            return Err(BloomFilterChunkRejectionReason::FirstChunkHasZeroBitCountOrProbeCount);
        }
        if chunk.bit_count > MAX_WORKER_STORE_PATH_BLOOM_FILTER_BIT_COUNT {
            return Err(BloomFilterChunkRejectionReason::FirstChunkFilterBitCountTooLarge);
        }
        Ok(Self {
            bit_count: chunk.bit_count,
            probe_count: chunk.probe_count,
            bits: BytesMut::new(),
        })
    }

    fn append(
        &mut self,
        chunk: &BloomFilterBitArrayChunk,
    ) -> Result<(), BloomFilterChunkRejectionReason> {
        if chunk.bit_count != self.bit_count || chunk.probe_count != self.probe_count {
            return Err(
                BloomFilterChunkRejectionReason::ChunkBitCountOrProbeCountDiffersFromFirstChunk,
            );
        }
        if chunk.offset != self.bits.len() as u64 {
            return Err(
                BloomFilterChunkRejectionReason::ChunkByteOffsetDoesNotContinuePreviousChunk,
            );
        }
        if (self.bits.len() + chunk.bits.len()) as u64 > self.bit_count.div_ceil(8) {
            return Err(BloomFilterChunkRejectionReason::ByteCountDoesNotMatchBitCount);
        }
        self.bits.extend_from_slice(&chunk.bits);
        Ok(())
    }

    fn finish(self) -> Result<WorkerStorePathBloomFilter, BloomFilterChunkRejectionReason> {
        if self.bits.len() as u64 != self.bit_count.div_ceil(8) {
            return Err(BloomFilterChunkRejectionReason::ByteCountDoesNotMatchBitCount);
        }
        Ok(WorkerStorePathBloomFilter {
            bit_count: self.bit_count,
            probe_count: self.probe_count,
            bits: self.bits,
        })
    }
}

impl From<BloomFilterBitArrayChunk> for ClientMessage {
    fn from(chunk: BloomFilterBitArrayChunk) -> Self {
        ClientMessage::WorkerLocalStoreBloomFilterChunk {
            bit_count: chunk.bit_count,
            probe_count: chunk.probe_count,
            offset: chunk.offset,
            bits: chunk.bits,
            is_final: chunk.is_final,
        }
    }
}

fn add_two_numbers_below_modulus_modulo_modulus_without_overflow(
    a: u64,
    b: u64,
    modulus: u64,
) -> u64 {
    let (sum, overflowed) = a.overflowing_add(b);
    if overflowed || sum >= modulus {
        sum.wrapping_sub(modulus)
    } else {
        sum
    }
}

fn bit_position_to_byte_index_and_mask(position: u64) -> (usize, u8) {
    let byte = usize::try_from(position / 8).unwrap_or(usize::MAX);
    (byte, 1u8 << (position % 8))
}

#[cfg(test)]
mod tests {
    use gradient_util::nix_hash::nix32_encode;

    use super::*;
    use crate::codec::to_bytes;
    use crate::messages::PROTO_VERSIONS;
    use crate::session::frame::SAFE_INFLIGHT_MESSAGE_SIZE;

    fn mix_counter_to_pseudo_random_u64(counter: u64) -> u64 {
        let mut z = counter.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn synthetic_store_path_hash_from_counter(counter: u64) -> String {
        let bytes: Vec<u8> = (0..3)
            .flat_map(|word| mix_counter_to_pseudo_random_u64(counter * 3 + word).to_le_bytes())
            .take(20)
            .collect();
        nix32_encode(&bytes)
    }

    fn probe_words_of_absent_entry(counter: u64) -> [u64; 2] {
        let base = u64::MAX / 2 + counter * 2;
        [
            mix_counter_to_pseudo_random_u64(base),
            mix_counter_to_pseudo_random_u64(base + 1),
        ]
    }

    fn build_filter_from_synthetic_store_path_hashes(
        count: u64,
    ) -> (WorkerStorePathBloomFilter, Vec<String>) {
        let hashes: Vec<String> = (0..count)
            .map(synthetic_store_path_hash_from_counter)
            .collect();
        (
            WorkerStorePathBloomFilter::from_store_path_hash_parts(&hashes),
            hashes,
        )
    }

    fn feed_chunks_into_collector_and_return_completed_filter(
        chunks: Vec<BloomFilterBitArrayChunk>,
    ) -> WorkerStorePathBloomFilter {
        let mut collector = WorkerStorePathBloomFilterChunkCollector::default();
        let mut complete = None;
        for chunk in chunks {
            complete = collector
                .add_chunk_and_return_complete_filter_if_final_chunk(chunk)
                .unwrap();
        }
        complete.unwrap()
    }

    #[test]
    fn all_100k_inserted_store_path_hashes_are_reported_contained() {
        let (filter, hashes) = build_filter_from_synthetic_store_path_hashes(100_000);

        let missing = hashes
            .iter()
            .filter(|hash| {
                let words = decode_store_path_hash_into_two_u64_words(hash).unwrap();
                !filter.probably_contains_probe_words(words)
            })
            .count();

        assert_eq!(missing, 0);
    }

    #[test]
    fn bloom_filter_false_positive_rate_for_one_million_never_inserted_entries_is_below_0_0002_at_twenty_bits_per_path()
     {
        let (filter, _) = build_filter_from_synthetic_store_path_hashes(100_000);
        let absent = 1_000_000;

        let false_positives = (0..absent)
            .filter(|&n| filter.probably_contains_probe_words(probe_words_of_absent_entry(n)))
            .count();

        let rate = false_positives as f64 / absent as f64;
        assert!(rate < 2e-4, "false positive rate {rate}");
    }

    #[test]
    fn filter_from_one_store_path_reports_no_false_positive_for_10000_uninserted_hashes() {
        let (filter, _) = build_filter_from_synthetic_store_path_hashes(1);

        let false_positives = (0..10_000)
            .filter(|&n| filter.probably_contains_probe_words(probe_words_of_absent_entry(n)))
            .count();

        assert_eq!(false_positives, 0);
    }

    #[test]
    fn two_point_five_megabyte_filter_splits_below_safe_message_size_and_reassembles_byte_identical()
     {
        let mut filter = WorkerStorePathBloomFilter::new_for_store_path_count(1_000_000);
        (0..100_000)
            .for_each(|n| filter.insert_probe_words_into_filter(probe_words_of_absent_entry(n)));
        assert_eq!(filter.bits.len(), 2_500_000);

        let chunks = filter
            .clone()
            .into_wire_chunks_of_at_most_max_chunk_size_with_last_final();

        assert_eq!(chunks.len(), 3);
        for (index, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.is_final, index == chunks.len() - 1);
            let encoded = to_bytes(&ClientMessage::from(chunk.clone()), *PROTO_VERSIONS.end())
                .unwrap()
                .len();
            assert!(
                encoded < SAFE_INFLIGHT_MESSAGE_SIZE,
                "chunk of {encoded} bytes"
            );
        }
        assert_eq!(
            feed_chunks_into_collector_and_return_completed_filter(chunks),
            filter
        );
    }

    #[test]
    fn reassembled_filter_reports_inserted_entry_as_contained() {
        let (filter, _) = build_filter_from_synthetic_store_path_hashes(1_000);
        let mut reassembled = feed_chunks_into_collector_and_return_completed_filter(
            filter.into_wire_chunks_of_at_most_max_chunk_size_with_last_final(),
        );
        let built_output = decode_store_path_hash_into_two_u64_words(
            &synthetic_store_path_hash_from_counter(5_000),
        )
        .unwrap();
        assert!(!reassembled.probably_contains_probe_words(built_output));

        reassembled.insert_probe_words_into_filter(built_output);

        assert!(reassembled.probably_contains_probe_words(built_output));
    }

    #[test]
    fn chunk_collector_rejects_invalid_chunk_sequences_with_matching_reason() {
        use BloomFilterChunkRejectionReason::*;
        let chunk = |bit_count, probe_count, offset, len, is_final| BloomFilterBitArrayChunk {
            bit_count,
            probe_count,
            offset,
            bits: Bytes::from(vec![0; len]),
            is_final,
        };
        let too_many_bits = MAX_WORKER_STORE_PATH_BLOOM_FILTER_BIT_COUNT + 1;
        let cases = [
            (
                "zero bit count",
                vec![chunk(0, 14, 0, 0, true)],
                FirstChunkHasZeroBitCountOrProbeCount,
            ),
            (
                "zero probe count",
                vec![chunk(24, 0, 0, 3, true)],
                FirstChunkHasZeroBitCountOrProbeCount,
            ),
            (
                "bit count above cap",
                vec![chunk(too_many_bits, 14, 0, 1, false)],
                FirstChunkFilterBitCountTooLarge,
            ),
            (
                "first chunk not at offset zero",
                vec![chunk(24, 14, 1, 1, true)],
                ChunkByteOffsetDoesNotContinuePreviousChunk,
            ),
            (
                "bit count changes",
                vec![chunk(24, 14, 0, 1, false), chunk(32, 14, 1, 1, false)],
                ChunkBitCountOrProbeCountDiffersFromFirstChunk,
            ),
            (
                "probe count changes",
                vec![chunk(24, 14, 0, 1, false), chunk(24, 13, 1, 1, false)],
                ChunkBitCountOrProbeCountDiffersFromFirstChunk,
            ),
            (
                "offset gap",
                vec![chunk(24, 14, 0, 1, false), chunk(24, 14, 2, 1, true)],
                ChunkByteOffsetDoesNotContinuePreviousChunk,
            ),
            (
                "offset overlap",
                vec![chunk(24, 14, 0, 2, false), chunk(24, 14, 1, 2, true)],
                ChunkByteOffsetDoesNotContinuePreviousChunk,
            ),
            (
                "bytes past bit count",
                vec![chunk(24, 14, 0, 2, false), chunk(24, 14, 2, 2, true)],
                ByteCountDoesNotMatchBitCount,
            ),
            (
                "short final chunk",
                vec![chunk(24, 14, 0, 2, true)],
                ByteCountDoesNotMatchBitCount,
            ),
        ];

        for (case, mut chunks, expected) in cases {
            let rejected = chunks.pop().unwrap();
            let mut collector = WorkerStorePathBloomFilterChunkCollector::default();
            for accepted in chunks {
                assert_eq!(
                    collector.add_chunk_and_return_complete_filter_if_final_chunk(accepted),
                    Ok(None),
                    "{case}"
                );
            }
            assert_eq!(
                collector.add_chunk_and_return_complete_filter_if_final_chunk(rejected),
                Err(expected),
                "{case}"
            );
        }
    }
}
