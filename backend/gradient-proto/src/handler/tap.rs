/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::events::EventBus;
use gradient_types::events::proto::{Direction, Message};
use gradient_wire::messages::{ArchivedClientMessage, ClientMessage, ServerMessage};
use gradient_wire::session::frame::{Inbound, MsgObserver};

pub(super) struct ServerTap {
    pub bus: EventBus,
    pub worker_id: String,
}

impl MsgObserver<ServerMessage> for ServerTap {
    fn sent(&self, msg: &ServerMessage, len: usize) {
        if !self.bus.wire_active() {
            return;
        }
        self.bus.publish(Message {
            direction: Direction::Server,
            worker_id: self.worker_id.clone(),
            message: msg.variant_name().to_owned(),
            job_id: msg.job_id().map(str::to_owned),
            len: Some(len),
        });
    }
}

pub(super) fn publish_inbound(bus: &EventBus, worker_id: &str, inbound: &Inbound<ClientMessage>) {
    if !bus.wire_active() {
        return;
    }
    let (job_id, len) = match inbound {
        Inbound::Control(msg) => (msg.job_id().map(str::to_owned), None),
        Inbound::Bulk(frame) => (bulk_job_id(frame.archived()), Some(frame.len())),
    };
    bus.publish(Message {
        direction: Direction::Client,
        worker_id: worker_id.to_owned(),
        message: inbound.variant_name().to_owned(),
        job_id,
        len,
    });
}

fn bulk_job_id(archived: &ArchivedClientMessage) -> Option<String> {
    match archived {
        ArchivedClientMessage::NarPush { job_id, .. }
        | ArchivedClientMessage::EvalCacheChunk { job_id, .. }
        | ArchivedClientMessage::LogChunk { job_id, .. } => Some(job_id.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inbound_control_message_is_published_as_a_proto_event() {
        let bus = EventBus::new(4);
        let mut rx = bus.subscribe_firehose();
        publish_inbound(&bus, "w1", &Inbound::Control(ClientMessage::RequestJobList));
        let env = rx.try_recv().unwrap();
        assert_eq!(env.event.name(), "proto.client.request_job_list");
    }

    #[test]
    fn without_a_firehose_nothing_is_built() {
        let bus = EventBus::new(4);
        let mut live = bus.subscribe();
        publish_inbound(&bus, "w1", &Inbound::Control(ClientMessage::RequestJobList));
        assert!(live.try_recv().is_err());
    }

    #[test]
    fn a_sent_message_is_published_with_its_job_and_length() {
        let bus = EventBus::new(4);
        let mut rx = bus.subscribe_firehose();
        let tap = ServerTap {
            bus: bus.clone(),
            worker_id: "w1".into(),
        };
        tap.sent(
            &ServerMessage::AbortJob {
                job_id: "j1".into(),
                reason: "r".into(),
            },
            42,
        );
        let env = rx.try_recv().unwrap();
        assert_eq!(env.event.name(), "proto.server.abort_job");
        assert_eq!(env.event.content()["job_id"], "j1");
        assert_eq!(env.event.content()["len"], 42);
    }
}
