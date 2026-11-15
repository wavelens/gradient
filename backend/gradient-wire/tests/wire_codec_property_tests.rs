/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;
use std::fmt::Debug;

use bytes::Bytes;
use gradient_wire::codec::{Proto, from_bytes, to_bytes};
use gradient_wire::guest::{AgentToWorker, GUEST_PROTO_VERSIONS, WorkerToAgent};
use gradient_wire::messages::{ClientMessage, PROTO_VERSIONS, ServerMessage};
use proptest::prelude::*;
use proptest::test_runner::{TestCaseError, TestCaseResult};

// The bytes include intentional duplication. Small values decode as lengths, option tags and booleans. Other byte values still appear.
fn small_byte_frame_strategy() -> impl Strategy<Value = Vec<u8>> {
    let byte = prop_oneof![3 => 0_u8..=3, 1 => any::<u8>()];
    (0_u8..64, proptest::collection::vec(byte, 0..256))
        .prop_map(|(tag, rest)| std::iter::once(tag).chain(rest).collect())
}

fn check_message_roundtrip_at_protocol_version<T: Proto>(
    frame: &[u8],
    version: u16,
) -> TestCaseResult {
    let mut input = Bytes::copy_from_slice(frame);
    let Ok(decoded) = T::decode(&mut input, version) else {
        return Ok(());
    };
    let encoded =
        to_bytes(&decoded, version).map_err(|error| TestCaseError::fail(error.to_string()))?;
    let again: T = from_bytes(encoded.clone(), version)
        .map_err(|error| TestCaseError::fail(error.to_string()))?;
    let reencoded =
        to_bytes(&again, version).map_err(|error| TestCaseError::fail(error.to_string()))?;
    prop_assert_eq!(reencoded, encoded);
    Ok(())
}

fn check_encoded_then_decoded_value_equals_original_at_protocol_version<
    T: Proto + PartialEq + Debug,
>(
    value: T,
    version: u16,
) -> TestCaseResult {
    let encoded =
        to_bytes(&value, version).map_err(|error| TestCaseError::fail(error.to_string()))?;
    prop_assert_eq!(from_bytes::<T>(encoded, version), Ok(value));
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 1024,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn decode_random_bytes_for_all_protocol_messages_without_panic_and_reencode_same_bytes(
        frame in small_byte_frame_strategy(),
        version in PROTO_VERSIONS,
        guest_version in GUEST_PROTO_VERSIONS,
    ) {
        check_message_roundtrip_at_protocol_version::<ClientMessage>(&frame, version)?;
        check_message_roundtrip_at_protocol_version::<ServerMessage>(&frame, version)?;
        check_message_roundtrip_at_protocol_version::<WorkerToAgent>(&frame, guest_version)?;
        check_message_roundtrip_at_protocol_version::<AgentToWorker>(&frame, guest_version)?;
    }

    #[test]
    fn test_round_trip_integers_strings_byte_buffers_options_lists_pairs_maps_and_floats_at_every_protocol_version(
        version in any::<u16>(),
        integers in (any::<u8>(), any::<u16>(), any::<u32>(), any::<u64>(), any::<i32>(), any::<i64>()),
        text in any::<String>(),
        buffer in proptest::collection::vec(any::<u8>(), 0..64),
        pairs in proptest::option::of(proptest::collection::vec((any::<String>(), any::<u64>()), 0..4)),
        map in proptest::collection::btree_map(any::<String>(), any::<u32>(), 0..4),
        flag in any::<bool>(),
        float_bits in any::<u32>(),
    ) {
        let (small, short, word, long, signed, signed_long) = integers;
        check_encoded_then_decoded_value_equals_original_at_protocol_version(small, version)?;
        check_encoded_then_decoded_value_equals_original_at_protocol_version(short, version)?;
        check_encoded_then_decoded_value_equals_original_at_protocol_version(word, version)?;
        check_encoded_then_decoded_value_equals_original_at_protocol_version(long, version)?;
        check_encoded_then_decoded_value_equals_original_at_protocol_version(signed, version)?;
        check_encoded_then_decoded_value_equals_original_at_protocol_version(signed_long, version)?;
        check_encoded_then_decoded_value_equals_original_at_protocol_version(text, version)?;
        check_encoded_then_decoded_value_equals_original_at_protocol_version(
            Bytes::from(buffer.clone()),
            version,
        )?;
        check_encoded_then_decoded_value_equals_original_at_protocol_version(buffer, version)?;
        check_encoded_then_decoded_value_equals_original_at_protocol_version(pairs, version)?;
        check_encoded_then_decoded_value_equals_original_at_protocol_version::<BTreeMap<String, u32>>(
            map,
            version,
        )?;
        check_encoded_then_decoded_value_equals_original_at_protocol_version(flag, version)?;
        let float = f32::from_bits(float_bits);
        let encoded = to_bytes(&float, version)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(from_bytes::<f32>(encoded, version).map(f32::to_bits), Ok(float_bits));
    }
}
