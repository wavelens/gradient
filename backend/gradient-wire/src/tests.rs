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

#[test]
fn split_evaluation_messages_round_trip_and_need_proto_40() {
    use crate::codec::{from_bytes, to_bytes};
    use crate::messages::{ClientMessage, ServerMessage};
    use crate::types::{EvalShard, EvalShardOutcome, EvalShardSpec, FinishedShard};

    let spec = EvalShardSpec {
        pattern: "legacyPackages.x86_64-linux.*".into(),
        set: vec!["legacyPackages".into(), "x86_64-linux".into()],
        children: Some(vec!["hello".into()]),
    };
    let client = [
        ClientMessage::EvalSourceReady {
            job_id: "eval:e".into(),
            store_path: "/nix/store/[hash130]-source".into(),
            fingerprint: "fp".into(),
        },
        ClientMessage::EvalShardsPlanned {
            job_id: "eval:e".into(),
            shards: vec![spec.clone()],
        },
        ClientMessage::EvalShardRequest {
            job_id: "eval:e:1".into(),
            have_source: true,
            finished: Some(FinishedShard {
                id: "s".into(),
                deferred: vec![spec],
            }),
        },
    ];
    for message in client {
        let bytes = to_bytes(&message, 40).unwrap();
        assert_eq!(from_bytes::<ClientMessage>(bytes, 40).unwrap(), message);
        assert!(to_bytes(&message, 39).is_err(), "{message:?}");
    }

    let outcomes = [
        EvalShardOutcome::Wait {
            retry_after_ms: 1000,
        },
        EvalShardOutcome::Source {
            store_path: "/nix/store/[hash130]-source".into(),
            fingerprint: "fp".into(),
        },
        EvalShardOutcome::Plan,
        EvalShardOutcome::Shard(EvalShard {
            id: "s".into(),
            pattern: "packages.*".into(),
            children: None,
            drv_paths: vec!["/nix/store/[hash131]-hello.drv".into()],
        }),
        EvalShardOutcome::Finished,
    ];
    for outcome in outcomes {
        let reply = ServerMessage::EvalShardReply {
            job_id: "eval:e".into(),
            outcome,
        };
        let bytes = to_bytes(&reply, 40).unwrap();
        assert_eq!(from_bytes::<ServerMessage>(bytes, 40).unwrap(), reply);
        assert!(to_bytes(&reply, 39).is_err());
    }
}
