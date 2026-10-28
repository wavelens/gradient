/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_pool::guest_channels::GuestChannelSignal;
use gradient_wire::messages::{GUEST_CHANNELS_SINCE, ServerMessage};

pub(super) const WORKER_TOO_OLD: &str = "the worker is too old for terminals";

pub(super) fn outbound_guest_channel(
    signal: GuestChannelSignal,
    version: u16,
) -> Result<ServerMessage, u64> {
    if version < GUEST_CHANNELS_SINCE {
        return Err(channel_of(&signal));
    }
    Ok(match signal {
        GuestChannelSignal::Open {
            job_id,
            channel_id,
            kind,
        } => ServerMessage::OpenGuestChannel {
            job_id,
            channel_id,
            kind,
        },
        GuestChannelSignal::Data { channel_id, data } => {
            ServerMessage::GuestChannelData { channel_id, data }
        }
        GuestChannelSignal::Resize {
            channel_id,
            cols,
            rows,
        } => ServerMessage::GuestChannelResize {
            channel_id,
            cols,
            rows,
        },
        GuestChannelSignal::Close { channel_id } => ServerMessage::CloseGuestChannel { channel_id },
    })
}

fn channel_of(signal: &GuestChannelSignal) -> u64 {
    match signal {
        GuestChannelSignal::Open { channel_id, .. }
        | GuestChannelSignal::Data { channel_id, .. }
        | GuestChannelSignal::Resize { channel_id, .. }
        | GuestChannelSignal::Close { channel_id } => *channel_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_wire::types::GuestChannelKind;

    #[test]
    fn an_older_worker_gets_no_guest_channel() {
        let open = GuestChannelSignal::Open {
            job_id: "exec:x".into(),
            channel_id: 3,
            kind: GuestChannelKind::Console,
        };
        assert_eq!(
            outbound_guest_channel(open, GUEST_CHANNELS_SINCE - 1),
            Err(3)
        );
    }

    #[test]
    fn each_signal_maps_to_its_message() {
        let resize = GuestChannelSignal::Resize {
            channel_id: 3,
            cols: 120,
            rows: 40,
        };
        assert_eq!(
            outbound_guest_channel(resize, GUEST_CHANNELS_SINCE),
            Ok(ServerMessage::GuestChannelResize {
                channel_id: 3,
                cols: 120,
                rows: 40
            })
        );
        assert_eq!(
            outbound_guest_channel(
                GuestChannelSignal::Close { channel_id: 3 },
                GUEST_CHANNELS_SINCE
            ),
            Ok(ServerMessage::CloseGuestChannel { channel_id: 3 })
        );
    }
}
