/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Regression for #110. The `/proto` cap must exceed the largest legitimate frame, a 512 KiB
/// `NarPush` plus its encoding overhead. It must stay well below tungstenite's 64 MiB default to keep a
/// malicious peer from forcing huge allocations.
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
