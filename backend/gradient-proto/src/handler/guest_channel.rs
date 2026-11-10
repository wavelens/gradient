/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_pool::guest_channels::GuestChannelSignal;
use gradient_wire::messages::{
    GUEST_CHANNEL_OUTPUT_FLOW_CONTROL_SINCE, GUEST_CHANNELS_SINCE, ServerMessage,
};

pub(super) const WORKER_TOO_OLD: &str = "the worker is too old for terminals";

pub(super) fn outbound_guest_channel(
    signal: GuestChannelSignal,
    version: u16,
) -> Result<Option<ServerMessage>, u64> {
    if version < GUEST_CHANNELS_SINCE {
        return Err(channel_of(&signal));
    }
    Ok(Some(match signal {
        GuestChannelSignal::Open {
            job_id,
            channel_id,
            kind,
            max_unread_output_bytes,
        } => ServerMessage::OpenGuestChannel {
            job_id,
            channel_id,
            kind,
            max_unread_output_bytes,
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
        GuestChannelSignal::OutputBytesRead { .. }
            if version < GUEST_CHANNEL_OUTPUT_FLOW_CONTROL_SINCE =>
        {
            return Ok(None);
        }
        GuestChannelSignal::OutputBytesRead { channel_id, total } => {
            ServerMessage::GuestChannelTotalOutputBytesRead { channel_id, total }
        }
    }))
}

fn channel_of(signal: &GuestChannelSignal) -> u64 {
    match signal {
        GuestChannelSignal::Open { channel_id, .. }
        | GuestChannelSignal::Data { channel_id, .. }
        | GuestChannelSignal::Resize { channel_id, .. }
        | GuestChannelSignal::Close { channel_id }
        | GuestChannelSignal::OutputBytesRead { channel_id, .. } => *channel_id,
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
            max_unread_output_bytes: None,
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
            Ok(Some(ServerMessage::GuestChannelResize {
                channel_id: 3,
                cols: 120,
                rows: 40
            }))
        );
        assert_eq!(
            outbound_guest_channel(
                GuestChannelSignal::Close { channel_id: 3 },
                GUEST_CHANNELS_SINCE
            ),
            Ok(Some(ServerMessage::CloseGuestChannel { channel_id: 3 }))
        );
    }

    #[test]
    fn guest_channel_signal_to_wire_message_mapping_depends_on_worker_protocol_version() {
        let read = || GuestChannelSignal::OutputBytesRead {
            channel_id: 3,
            total: 4096,
        };
        assert_eq!(
            outbound_guest_channel(read(), GUEST_CHANNEL_OUTPUT_FLOW_CONTROL_SINCE - 1),
            Ok(None)
        );
        assert_eq!(
            outbound_guest_channel(read(), GUEST_CHANNEL_OUTPUT_FLOW_CONTROL_SINCE),
            Ok(Some(ServerMessage::GuestChannelTotalOutputBytesRead {
                channel_id: 3,
                total: 4096
            }))
        );
        let open = GuestChannelSignal::Open {
            job_id: "exec:x".into(),
            channel_id: 3,
            kind: GuestChannelKind::Console,
            max_unread_output_bytes: Some(1 << 20),
        };
        assert_eq!(
            outbound_guest_channel(open, GUEST_CHANNELS_SINCE),
            Ok(Some(ServerMessage::OpenGuestChannel {
                job_id: "exec:x".into(),
                channel_id: 3,
                kind: GuestChannelKind::Console,
                max_unread_output_bytes: Some(1 << 20)
            }))
        );
    }
}
