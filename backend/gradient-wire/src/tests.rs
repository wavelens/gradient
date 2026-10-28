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

#[test]
fn running_jobs_round_trips_and_is_refused_to_an_older_peer() {
    use crate::codec::{EncodeError, from_bytes, to_bytes};
    use crate::messages::{ClientMessage, JobKind, RUNNING_JOBS_SINCE, RunningAssignment};

    let msg = ClientMessage::RunningJobs {
        assignments: vec![RunningAssignment {
            assignment_id: "a1".into(),
            kind: JobKind::Build,
        }],
    };
    let bytes = to_bytes(&msg, RUNNING_JOBS_SINCE).unwrap();
    assert_eq!(
        from_bytes::<ClientMessage>(bytes, RUNNING_JOBS_SINCE).unwrap(),
        msg
    );
    assert!(matches!(
        to_bytes(&msg, RUNNING_JOBS_SINCE - 1),
        Err(EncodeError::NewerThanPeer { .. })
    ));
}

#[test]
fn worker_metrics_from_an_older_peer_carry_no_lifetime() {
    use crate::codec::{from_bytes, to_bytes};
    use crate::messages::{ClientMessage, RUNNING_JOBS_SINCE};

    let metrics = |lifetime_left_secs| ClientMessage::WorkerMetrics {
        cpu_usage_pct: 1.0,
        ram_free_mb: 2,
        disk_speed_mbps: None,
        upload_speed_mbps: None,
        download_speed_mbps: None,
        build_peak_ram_mb: None,
        lifetime_left_secs,
        exec: None,
    };
    let old = RUNNING_JOBS_SINCE - 1;
    let decoded =
        from_bytes::<ClientMessage>(to_bytes(&metrics(Some(60)), old).unwrap(), old).unwrap();
    assert_eq!(decoded, metrics(None));
}

#[test]
fn worker_capabilities_from_the_previous_version_carry_no_exec_inventory() {
    use crate::codec::{from_bytes, to_bytes};
    use crate::messages::{ClientMessage, PROTO_VERSIONS};
    use crate::types::{DeviceClass, ExecInventory};

    let caps = ClientMessage::WorkerCapabilities {
        architectures: vec!["x86_64-linux".into()],
        system_features: vec![],
        max_concurrent_builds: 1,
        cpu_count: 1,
        ram_total_mb: 1,
        cpu_core_score: 1,
        zone: None,
        endpoint: None,
        build_cores: None,
        exec: Some(Box::new(ExecInventory {
            labels: vec!["w1".into()],
            devices: vec![DeviceClass {
                name: "gpu:h100".into(),
                total: 8,
                free: 6,
            }],
            ..Default::default()
        })),
    };
    let newest = *PROTO_VERSIONS.end();
    assert_eq!(
        from_bytes::<ClientMessage>(to_bytes(&caps, newest).unwrap(), newest).unwrap(),
        caps
    );
    let older = from_bytes::<ClientMessage>(to_bytes(&caps, 42).unwrap(), 42).unwrap();
    assert!(matches!(
        older,
        ClientMessage::WorkerCapabilities { exec: None, .. }
    ));
}

#[test]
fn an_exec_job_and_a_phase_round_trip() {
    use std::collections::BTreeMap;

    use crate::codec::{from_bytes, to_bytes};
    use crate::messages::{ClientMessage, PROTO_VERSIONS};
    use crate::types::{DeviceRequest, ExecJob, ExecRequirement, Job, JobContext};

    let newest = *PROTO_VERSIONS.end();
    let job = Job::Exec(Box::new(ExecJob {
        program: "/nix/store/aaaa-check".into(),
        args: vec!["--x".into()],
        env: BTreeMap::from([("WANDB_PROJECT".into(), "llm".into())]),
        image: None,
        requirement: ExecRequirement {
            devices: vec![DeviceRequest {
                name: "gpu:h100".into(),
                count: 8,
            }],
            ..Default::default()
        },
        context: JobContext::default(),
        secrets: vec![],
        keep_running: false,
        timeout_secs: Some(1),
        max_silent_secs: None,
        attempt: 1,
    }));
    assert_eq!(
        from_bytes::<Job>(to_bytes(&job, newest).unwrap(), newest).unwrap(),
        job
    );
    let phase = ClientMessage::JobPhase {
        job_id: "exec:x".into(),
        name: "epoch-3".into(),
        payload: vec![1, 2],
    };
    assert_eq!(phase.job_id(), Some("exec:x"));
    assert_eq!(phase.variant_name(), "JobPhase");
}

#[test]
fn pre_warm_vms_round_trips_and_names_no_job() {
    use crate::codec::{from_bytes, to_bytes};
    use crate::messages::{PROTO_VERSIONS, ServerMessage};

    let newest = *PROTO_VERSIONS.end();
    let msg = ServerMessage::PreWarmVms { count: 3 };
    assert_eq!(
        from_bytes::<ServerMessage>(to_bytes(&msg, newest).unwrap(), newest).unwrap(),
        msg
    );
    assert_eq!((msg.job_id(), msg.variant_name()), (None, "PreWarmVms"));
}

#[test]
fn guest_channel_messages_round_trip_and_stay_off_older_sessions() {
    use crate::codec::{from_bytes, to_bytes};
    use crate::messages::{ClientMessage, GUEST_CHANNELS_SINCE, GuestChannelKind, ServerMessage};

    let open = ServerMessage::OpenGuestChannel {
        job_id: "exec:x".into(),
        channel_id: 7,
        kind: GuestChannelKind::Forward { port: 6006 },
    };
    assert_eq!(
        from_bytes::<ServerMessage>(
            to_bytes(&open, GUEST_CHANNELS_SINCE).unwrap(),
            GUEST_CHANNELS_SINCE
        )
        .unwrap(),
        open
    );
    assert!(to_bytes(&open, GUEST_CHANNELS_SINCE - 1).is_err());
    assert_eq!(open.job_id(), Some("exec:x"));
    let closed = ClientMessage::CloseGuestChannel {
        channel_id: 7,
        exit_code: Some(0),
        message: None,
    };
    assert_eq!(
        from_bytes::<ClientMessage>(
            to_bytes(&closed, GUEST_CHANNELS_SINCE).unwrap(),
            GUEST_CHANNELS_SINCE
        )
        .unwrap(),
        closed
    );
}

#[test]
fn a_job_secret_credential_round_trips() {
    use crate::codec::{from_bytes, to_bytes};
    use crate::messages::{CredentialKind, PROTO_VERSIONS, ServerMessage};

    let newest = *PROTO_VERSIONS.end();
    let msg = ServerMessage::Credential {
        kind: CredentialKind::JobSecret {
            job_id: "exec:1".into(),
            name: "wandb-token".into(),
        },
        data: b"v".to_vec().into(),
    };
    assert_eq!(
        from_bytes::<ServerMessage>(to_bytes(&msg, newest).unwrap(), newest).unwrap(),
        msg
    );
}

#[test]
fn terminal_bytes_are_never_logged() {
    use crate::messages::{ClientMessage, ServerMessage};

    assert!(
        ServerMessage::GuestChannelData {
            channel_id: 1,
            data: b"hunter2".to_vec()
        }
        .carries_secret()
    );
    assert!(
        ClientMessage::GuestChannelData {
            channel_id: 1,
            data: b"hunter2".to_vec()
        }
        .carries_secret()
    );
}

#[test]
fn an_exec_output_upload_and_the_job_names_cross_the_wire() {
    use crate::codec::{from_bytes, to_bytes};
    use crate::messages::{ClientMessage, JobContext, PROTO_VERSIONS};
    use crate::types::UploadObject;

    let newest = *PROTO_VERSIONS.end();
    let request = ClientMessage::UploadRequest {
        job_id: "exec:x".into(),
        request_id: 1,
        object: UploadObject::ExecOutput {
            job: "x".into(),
            path: "model/weights.bin".into(),
        },
        size: 3,
    };
    assert_eq!(
        from_bytes::<ClientMessage>(to_bytes(&request, newest).unwrap(), newest).unwrap(),
        request
    );
    let context = JobContext {
        parent_job: Some("p".into()),
        project_name: "infra".into(),
        task_name: "master".into(),
        ..Default::default()
    };
    assert_eq!(
        from_bytes::<JobContext>(to_bytes(&context, newest).unwrap(), newest).unwrap(),
        context
    );
    let older: JobContext = from_bytes(to_bytes(&context, 46).unwrap(), 46).unwrap();
    assert_eq!((older.parent_job, older.project_name.as_str()), (None, ""));
}

#[test]
fn a_flake_job_carries_its_flake_directory() {
    use crate::codec::{from_bytes, to_bytes};
    use crate::types::{FlakeJob, FlakeSource, FlakeStep};

    let job = FlakeJob {
        steps: vec![FlakeStep::FetchFlake],
        source: FlakeSource::Repository {
            url: "https://h/o/r".into(),
            commit: "0".repeat(40),
        },
        wildcards: vec!["*".into()],
        timeout_secs: None,
        input_overrides: vec![],
        input_update: None,
        flake_dir: Some("nix".into()),
    };
    assert_eq!(
        from_bytes::<FlakeJob>(to_bytes(&job, 48).unwrap(), 48).unwrap(),
        job
    );
    let older: FlakeJob = from_bytes(to_bytes(&job, 47).unwrap(), 47).unwrap();
    assert_eq!(older.flake_dir, None);
}

#[test]
fn an_accept_from_the_previous_version_carries_no_endpoint() {
    use crate::codec::{from_bytes, to_bytes};
    use crate::messages::{ClientMessage, PROTO_VERSIONS};

    let accept = ClientMessage::AssignJobResponse {
        job_id: "exec:a".into(),
        accepted: true,
        reason: None,
        endpoint: Some("10.233.0.2".into()),
    };
    let newest = *PROTO_VERSIONS.end();
    assert_eq!(
        from_bytes::<ClientMessage>(to_bytes(&accept, newest).unwrap(), newest).unwrap(),
        accept
    );
    let older = from_bytes::<ClientMessage>(to_bytes(&accept, 48).unwrap(), 48).unwrap();
    assert!(matches!(
        older,
        ClientMessage::AssignJobResponse { endpoint: None, .. }
    ));
}

#[test]
fn a_discovered_derivation_names_the_outputs_of_each_input_since_version_50() {
    use crate::codec::{from_bytes, to_bytes};
    use crate::messages::PROTO_VERSIONS;
    use crate::types::DiscoveredDerivation;

    let newest = *PROTO_VERSIONS.end();
    let discovered = DiscoveredDerivation {
        drv_path: "a.drv".into(),
        dependencies: vec!["b.drv".into()],
        dependency_outputs: vec![vec!["out".into(), "dev".into()]],
        ..Default::default()
    };
    assert_eq!(
        from_bytes::<DiscoveredDerivation>(to_bytes(&discovered, newest).unwrap(), newest).unwrap(),
        discovered
    );
    let older: DiscoveredDerivation = from_bytes(to_bytes(&discovered, 49).unwrap(), 49).unwrap();
    assert_eq!(older.dependencies, discovered.dependencies);
    assert!(older.dependency_outputs.is_empty());
}
