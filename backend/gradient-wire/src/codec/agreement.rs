/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::ops::RangeInclusive;

use bytes::{BufMut, Bytes, BytesMut};

const PREFIX: &[u8; 4] = b"GRAD";

pub fn version_frame(ours: &RangeInclusive<u16>) -> Bytes {
    let mut out = BytesMut::with_capacity(8);
    out.put_slice(PREFIX);
    out.put_u16_le(*ours.start());
    out.put_u16_le(*ours.end());
    out.freeze()
}

pub fn agree(ours: &RangeInclusive<u16>, frame: &[u8]) -> Result<u16, String> {
    let Some(&[a, b, c, d]) = frame.strip_prefix(PREFIX) else {
        return Err(format!("peer protocol is older than {}", ours.start()));
    };

    let peer = u16::from_le_bytes([a, b])..=u16::from_le_bytes([c, d]);
    let version = (*ours.end()).min(*peer.end());
    if version < (*ours.start()).max(*peer.start()) {
        return Err(format!(
            "no shared protocol version: ours {ours:?}, peer {peer:?}"
        ));
    }

    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::{agree, version_frame};

    #[test]
    fn both_sides_use_the_highest_shared_version() {
        assert_eq!(agree(&(27..=30), &version_frame(&(28..=31))), Ok(30));
        assert_eq!(agree(&(28..=31), &version_frame(&(27..=30))), Ok(30));
    }

    #[test]
    fn ranges_without_overlap_are_refused_with_both_ranges() {
        assert_eq!(
            agree(&(27..=27), &version_frame(&(30..=31))),
            Err("no shared protocol version: ours 27..=27, peer 30..=31".to_owned())
        );
    }

    #[test]
    fn a_first_frame_without_the_prefix_is_an_older_peer() {
        assert_eq!(
            agree(&(27..=28), b"\x00\x01rkyv-greeting"),
            Err("peer protocol is older than 27".to_owned())
        );
    }
}
