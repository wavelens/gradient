/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::events::EventBus;
use gradient_types::events::proto::{Direction, Message};
use gradient_wire::messages::{ClientMessage, ServerMessage};
use gradient_wire::session::frame::MsgObserver;

pub(super) struct ProtoTap {
    pub bus: EventBus,
    pub worker_id: String,
}

impl ProtoTap {
    fn publish(&self, direction: Direction, message: &str, job_id: Option<&str>, len: usize) {
        if !self.bus.wire_active() {
            return;
        }

        self.bus.publish(Message {
            direction,
            worker_id: self.worker_id.clone(),
            message: message.to_owned(),
            job_id: job_id.map(str::to_owned),
            len: Some(len),
        });
    }
}

impl MsgObserver<ServerMessage> for ProtoTap {
    fn observe(&self, msg: &ServerMessage, len: usize) {
        self.publish(Direction::Server, msg.variant_name(), msg.job_id(), len);
    }
}

impl MsgObserver<ClientMessage> for ProtoTap {
    fn observe(&self, msg: &ClientMessage, len: usize) {
        self.publish(Direction::Client, msg.variant_name(), msg.job_id(), len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tap(bus: &EventBus) -> ProtoTap {
        ProtoTap {
            bus: bus.clone(),
            worker_id: "w1".into(),
        }
    }

    #[test]
    fn a_received_message_is_published_as_a_proto_event() {
        let bus = EventBus::new(4);
        let mut rx = bus.subscribe_firehose();
        tap(&bus).observe(&ClientMessage::RequestJobList, 1);
        let env = rx.try_recv().unwrap();
        assert_eq!(env.event.name(), "proto.client.request_job_list");
    }

    #[test]
    fn without_a_firehose_nothing_is_built() {
        let bus = EventBus::new(4);
        let mut live = bus.subscribe();
        tap(&bus).observe(&ClientMessage::RequestJobList, 1);
        assert!(live.try_recv().is_err());
    }

    #[test]
    fn a_sent_message_is_published_with_its_job_and_length() {
        let bus = EventBus::new(4);
        let mut rx = bus.subscribe_firehose();
        tap(&bus).observe(
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
