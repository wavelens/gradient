/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_pool::guest_channels::GuestChannelSignal;
use gradient_wire::messages::ServerMessage;

pub(super) fn outbound_guest_channel(signal: GuestChannelSignal) -> ServerMessage {
    match signal {
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
        GuestChannelSignal::OutputBytesRead { channel_id, total } => {
            ServerMessage::GuestChannelTotalOutputBytesRead { channel_id, total }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_wire::types::GuestChannelKind;

    #[test]
    fn each_signal_maps_to_its_message() {
        let resize = GuestChannelSignal::Resize {
            channel_id: 3,
            cols: 120,
            rows: 40,
        };
        assert_eq!(
            outbound_guest_channel(resize),
            ServerMessage::GuestChannelResize {
                channel_id: 3,
                cols: 120,
                rows: 40
            }
        );
        assert_eq!(
            outbound_guest_channel(GuestChannelSignal::Close { channel_id: 3 }),
            ServerMessage::CloseGuestChannel { channel_id: 3 }
        );
        assert_eq!(
            outbound_guest_channel(GuestChannelSignal::OutputBytesRead {
                channel_id: 3,
                total: 4096,
            }),
            ServerMessage::GuestChannelTotalOutputBytesRead {
                channel_id: 3,
                total: 4096
            }
        );
        let open = GuestChannelSignal::Open {
            job_id: "exec:x".into(),
            channel_id: 3,
            kind: GuestChannelKind::Console,
            max_unread_output_bytes: Some(1 << 20),
        };
        assert_eq!(
            outbound_guest_channel(open),
            ServerMessage::OpenGuestChannel {
                job_id: "exec:x".into(),
                channel_id: 3,
                kind: GuestChannelKind::Console,
                max_unread_output_bytes: Some(1 << 20)
            }
        );
    }
}
