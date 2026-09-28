/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Regression for #110: the `/proto` WebSocket cap must:
/// - exceed the largest legitimate frame (`NarPush` at 512 KiB plus rkyv
///   overhead, plus headroom for `LogChunk`/`CacheQuery` arrays), and
/// - stay well below tungstenite's 64 MiB default so a malicious peer can't
///   ask the server to allocate gigabytes from a single send.
#[test]
fn max_proto_message_size_is_sane() {
    use crate::session::frame::{BULK_CHUNK_SIZE, MAX_PROTO_MESSAGE_SIZE};
    const _: () = {
        assert!(
            MAX_PROTO_MESSAGE_SIZE >= BULK_CHUNK_SIZE * 2,
            "must fit a NarPush chunk plus framing/metadata",
        );
        assert!(
            MAX_PROTO_MESSAGE_SIZE <= 16 * 1024 * 1024,
            "guard against accidental relaxation back toward defaults",
        );
    };
}
