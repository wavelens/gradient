/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use bytes::Bytes;
use gradient_wire::codec::{DecodeError, EncodeError, Proto, from_bytes, to_bytes};

#[derive(Proto, Debug, Clone, PartialEq, Default)]
struct Offer {
    id: String,
    #[proto(28, default)]
    features: Vec<String>,
}

#[derive(Proto, Debug, Clone, PartialEq)]
struct Pinned {
    id: String,
    #[proto(29)]
    system: String,
}

#[derive(Proto, Debug, Clone, PartialEq, Default)]
struct Holder {
    #[proto(28, default)]
    pinned: Option<Pinned>,
}

#[derive(Proto, Debug, Clone, PartialEq)]
#[proto(oldest = 27)]
enum Message {
    Hello {
        id: String,
    },
    Ping,
    Wrapped(Offer),
    #[proto(28)]
    Bye {
        reason: String,
    },
}

fn at<T: Proto>(value: &T, version: u16) -> Bytes {
    to_bytes(value, version).expect("encodes")
}

#[test]
fn a_field_newer_than_the_peer_is_left_out_and_read_as_its_default() {
    let offer = Offer {
        id: "j".into(),
        features: vec!["kvm".into()],
    };
    let old = from_bytes::<Offer>(at(&offer, 27), 27).expect("decodes");
    assert_eq!(
        old,
        Offer {
            id: "j".into(),
            ..Default::default()
        }
    );
    assert_eq!(from_bytes::<Offer>(at(&offer, 28), 28), Ok(offer));
}

#[test]
fn a_required_field_raises_the_oldest_version() {
    assert_eq!((Offer::OLDEST, Offer::NEWEST), (0, 28));
    assert_eq!((Pinned::OLDEST, Pinned::NEWEST), (29, 29));
}

#[test]
fn a_default_field_holding_a_newer_type_raises_the_oldest_version() {
    assert_eq!(Holder::OLDEST, 29);
}

#[test]
fn the_enum_range_spans_its_oldest_attribute_and_newest_variant() {
    assert_eq!((Message::OLDEST, Message::NEWEST), (27, 28));
}

#[test]
fn every_variant_round_trips() {
    for message in [
        Message::Hello { id: "w".into() },
        Message::Ping,
        Message::Wrapped(Offer::default()),
        Message::Bye {
            reason: "done".into(),
        },
    ] {
        assert_eq!(from_bytes::<Message>(at(&message, 28), 28), Ok(message));
    }
}

#[test]
fn a_variant_newer_than_the_peer_is_refused_on_send() {
    let bye = Message::Bye {
        reason: "done".into(),
    };
    assert_eq!(
        to_bytes(&bye, 27),
        Err(EncodeError::NewerThanPeer {
            variant: "Bye",
            since: 28,
            version: 27
        })
    );
}

#[test]
fn a_variant_newer_than_the_agreed_version_is_rejected_on_receive() {
    let bytes = at(&Message::Bye { reason: "x".into() }, 28);
    assert!(matches!(
        from_bytes::<Message>(bytes, 27),
        Err(DecodeError::UnknownVariant {
            tag: 3,
            version: 27,
            ..
        })
    ));
}

#[test]
fn the_shape_leaves_out_names_and_versions_the_peer_lacks() {
    let mut old = String::new();
    Message::describe(27, &mut old);
    assert_eq!(old, "[0:{str},1:{},2:{{str}}]");

    let mut new = String::new();
    Message::describe(28, &mut new);
    assert_eq!(new, "[0:{str},1:{},2:{{str,seq<str>}},3:{str}]");
}
