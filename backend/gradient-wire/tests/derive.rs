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

#[derive(Proto, Debug, Clone, PartialEq)]
#[proto(oldest = 27)]
enum Grown {
    Hello,
    #[proto(28)]
    Later {
        #[proto(28)]
        detail: String,
    },
}

#[derive(Proto, Debug, Clone, PartialEq, Default)]
#[proto(removed(28, Option<f32>))]
struct Trimmed {
    id: String,
    #[proto(28, default)]
    upload: Option<f32>,
}

#[derive(Proto, Debug, Clone, PartialEq, Default)]
struct BeforeTrim {
    id: String,
    peak: Option<f32>,
}

#[derive(Proto, Debug, Clone, PartialEq)]
#[proto(oldest = 27)]
enum Heartbeat {
    #[proto(removed(28, Option<f32>))]
    Load {
        cpu: u32,
        #[proto(28, default)]
        upload: Option<f32>,
    },
}

#[derive(Proto, Debug, Clone, PartialEq)]
#[proto(oldest = 27)]
enum HeartbeatBeforeTrim {
    Load { cpu: u32, peak: Option<f32> },
}

#[derive(Proto, Debug, Clone, PartialEq)]
#[proto(oldest = 27)]
enum HeartbeatAfterTrim {
    Load { cpu: u32, upload: Option<f32> },
}

fn at<T: Proto>(value: &T, version: u16) -> Bytes {
    to_bytes(value, version).expect("encodes")
}

fn shape<T: Proto>(version: u16) -> String {
    let mut out = String::new();
    T::describe(version, &mut out);
    out
}

#[test]
fn a_removed_field_keeps_its_place_for_older_peers_only() {
    let trimmed = Trimmed {
        id: "j".into(),
        upload: Some(8.0),
    };
    let unset = BeforeTrim {
        id: "j".into(),
        peak: None,
    };
    assert_eq!(at(&trimmed, 27), at(&unset, 27));
    assert_eq!(shape::<Trimmed>(27), shape::<BeforeTrim>(27));

    let sent_by_old_peer = BeforeTrim {
        peak: Some(3.0),
        ..unset
    };
    assert_eq!(
        from_bytes::<Trimmed>(at(&sent_by_old_peer, 27), 27),
        Ok(Trimmed {
            id: "j".into(),
            upload: None,
        })
    );
    assert_eq!(from_bytes::<Trimmed>(at(&trimmed, 28), 28), Ok(trimmed));
    assert_eq!((Trimmed::OLDEST, Trimmed::NEWEST), (0, 28));
}

#[test]
fn a_variant_drops_a_removed_field_from_the_version_that_removed_it() {
    assert_eq!(shape::<Heartbeat>(27), shape::<HeartbeatBeforeTrim>(27));
    assert_eq!(shape::<Heartbeat>(28), shape::<HeartbeatAfterTrim>(28));

    let load = Heartbeat::Load {
        cpu: 4,
        upload: Some(8.0),
    };
    assert_eq!(
        at(&load, 27),
        at(&HeartbeatBeforeTrim::Load { cpu: 4, peak: None }, 27)
    );
    assert_eq!(from_bytes::<Heartbeat>(at(&load, 28), 28), Ok(load));
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
fn a_field_as_new_as_its_variant_keeps_the_oldest_version() {
    assert_eq!((Grown::OLDEST, Grown::NEWEST), (27, 28));
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
