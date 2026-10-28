/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use anyhow::{Context, Result, anyhow};
use bytes::Bytes;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, Stream, StreamExt};
use gradient_secret::SecretBytes;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::codec::agreement::{agree, version_frame};
use crate::codec::{Proto, from_bytes, to_bytes};
use crate::types::GuestChannelKind;

pub const GUEST_PROTO_VERSIONS: RangeInclusive<u16> = 1..=4;
pub const GUEST_CHANNELS_AGENT_SINCE: u16 = 2;
pub const GUEST_JOB_SECRETS_SINCE: u16 = 2;
pub const GUEST_UPLOADS_SINCE: u16 = 3;
pub const AGENT_VSOCK_PORT: u32 = 7000;
pub const AGENT_RELAY_PORT_BASE: u32 = 7100;

const MAX_GUEST_FRAME: usize = 16 * 1024 * 1024;

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum WorkerToAgent {
    Attach {
        output_offset: u64,
    },
    RunJob(Box<RunJob>),
    Kill,
    #[proto(2)]
    OpenChannel {
        channel_id: u64,
        kind: GuestChannelKind,
    },
    #[proto(2)]
    ChannelData {
        channel_id: u64,
        data: Vec<u8>,
    },
    #[proto(2)]
    CloseChannel {
        channel_id: u64,
    },
    #[proto(2)]
    Resize {
        channel_id: u64,
        cols: u16,
        rows: u16,
    },
    #[proto(3)]
    UploadSettled {
        upload_id: u64,
        error: Option<String>,
    },
}

#[derive(Proto, Debug, Clone, PartialEq, Default)]
pub struct RunJob {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub context: ContextLayers,
    pub last_phase_payload: Vec<u8>,
    pub secrets: Vec<GuestSecret>,
    pub mounts: Vec<GuestMount>,
    pub groups: Vec<String>,
    pub network: Option<GuestNetwork>,
    pub relays: Vec<GuestRelay>,
    pub usb_bus_ids: Vec<String>,
    #[proto(2, default)]
    pub program_pty: bool,
}

#[derive(Proto, Debug, Clone, PartialEq, Default)]
pub struct ContextLayers {
    pub worker: BTreeMap<String, String>,
    pub cluster: BTreeMap<String, String>,
    pub job: BTreeMap<String, String>,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct GuestSecret {
    pub name: String,
    pub value: SecretBytes,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct GuestMount {
    pub tag: String,
    pub path: String,
    pub read_only: bool,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct GuestNetwork {
    pub address: String,
    pub prefix: u8,
    pub gateway: String,
    pub nameservers: Vec<String>,
    #[proto(4, default)]
    pub mac: String,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct GuestRelay {
    pub name: String,
    pub port: u32,
    pub kind: GuestRelayKind,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum GuestRelayKind {
    Pty,
    Tcp { listen: u16 },
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum AgentToWorker {
    Ready {
        agent_version: String,
    },
    ProgramStarted,
    Output {
        data: Vec<u8>,
    },
    OutputDropped {
        bytes: u64,
    },
    Phase {
        name: String,
        payload: Vec<u8>,
    },
    SetupFailed {
        message: String,
    },
    Exited {
        code: i32,
    },
    JobLost,
    #[proto(2)]
    ChannelData {
        channel_id: u64,
        data: Vec<u8>,
    },
    #[proto(2)]
    CloseChannel {
        channel_id: u64,
        exit_code: Option<i32>,
        message: Option<String>,
    },
    #[proto(2)]
    JobSecrets {
        secrets: Vec<GuestSecret>,
        token: String,
    },
    #[proto(3)]
    UploadStart {
        upload_id: u64,
        path: String,
        size: u64,
    },
    #[proto(3)]
    UploadChunk {
        upload_id: u64,
        data: Vec<u8>,
    },
    #[proto(3)]
    UploadEnd {
        upload_id: u64,
    },
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum SupervisorRequest {
    PrepareMount {
        tag: String,
        dir: String,
        read_only: bool,
    },
    PrepareTap {
        name: String,
        bridge: String,
    },
    BindUsb {
        vendor_product: String,
    },
    UnbindUsb {
        bus_id: String,
    },
    RelayDevice {
        path: String,
        guest_port: u32,
    },
    RelayUsbip {
        guest_port: u32,
    },
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum SupervisorReply {
    Done,
    MountSocket { path: String },
    UsbBound { bus_id: String },
    Failed { message: String },
}

pub struct GuestConnection<S> {
    frames: Framed<S, LengthDelimitedCodec>,
    version: u16,
}

impl<S: AsyncRead + AsyncWrite + Unpin> GuestConnection<S> {
    pub async fn agree(stream: S) -> Result<Self> {
        Self::agree_within(stream, &GUEST_PROTO_VERSIONS).await
    }

    async fn agree_within(stream: S, ours: &RangeInclusive<u16>) -> Result<Self> {
        let codec = LengthDelimitedCodec::builder()
            .max_frame_length(MAX_GUEST_FRAME)
            .new_codec();
        let mut frames = Framed::new(stream, codec);
        frames
            .send(version_frame(ours))
            .await
            .context("sending the guest protocol range")?;
        let peer = frames
            .next()
            .await
            .ok_or_else(|| anyhow!("guest peer closed before the version agreement"))?
            .context("reading the guest protocol range")?;
        let version = agree(ours, &peer).map_err(|e| anyhow!(e))?;
        Ok(Self { frames, version })
    }

    pub fn version(&self) -> u16 {
        self.version
    }

    pub async fn send<T: Proto>(&mut self, msg: &T) -> Result<()> {
        let bytes = to_bytes(msg, self.version)?;
        self.frames.send(bytes).await?;
        Ok(())
    }

    pub async fn recv<T: Proto>(&mut self) -> Result<Option<T>> {
        self.incoming().next().await.transpose()
    }

    pub fn incoming<T: Proto>(&mut self) -> impl Stream<Item = Result<T>> + Unpin + '_ {
        let version = self.version;
        (&mut self.frames).map(move |frame| Ok(from_bytes(frame?.freeze(), version)?))
    }

    pub fn split(self) -> (GuestSender<S>, GuestReceiver<S>) {
        let (sink, stream) = self.frames.split();
        let version = self.version;
        (
            GuestSender {
                frames: sink,
                version,
            },
            GuestReceiver {
                frames: stream,
                version,
            },
        )
    }
}

pub struct GuestSender<S> {
    frames: SplitSink<Framed<S, LengthDelimitedCodec>, Bytes>,
    version: u16,
}

impl<S: AsyncRead + AsyncWrite + Unpin> GuestSender<S> {
    pub async fn send<T: Proto>(&mut self, msg: &T) -> Result<()> {
        let bytes = to_bytes(msg, self.version)?;
        self.frames.send(bytes).await?;
        Ok(())
    }
}

pub struct GuestReceiver<S> {
    frames: SplitStream<Framed<S, LengthDelimitedCodec>>,
    version: u16,
}

impl<S: AsyncRead + AsyncWrite + Unpin> GuestReceiver<S> {
    pub async fn recv<T: Proto>(&mut self) -> Result<Option<T>> {
        let version = self.version;
        match self.frames.next().await {
            Some(frame) => Ok(Some(from_bytes(frame?.freeze(), version)?)),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_job() -> RunJob {
        RunJob {
            program: "/nix/store/abc-train/bin/train".into(),
            args: vec!["--epochs".into(), "3".into()],
            env: BTreeMap::from([("RUST_LOG".into(), "info".into())]),
            context: ContextLayers {
                worker: BTreeMap::from([("zone".into(), "fra1".into())]),
                ..ContextLayers::default()
            },
            last_phase_payload: b"step=2".to_vec(),
            secrets: vec![GuestSecret {
                name: "wandb-token".into(),
                value: SecretBytes::from(b"s3cr3t".as_slice()),
            }],
            mounts: vec![GuestMount {
                tag: "datasets".into(),
                path: "/mnt/datasets".into(),
                read_only: true,
            }],
            groups: vec!["kvm".into()],
            network: Some(GuestNetwork {
                address: "10.233.0.3".into(),
                prefix: 16,
                gateway: "10.233.0.1".into(),
                nameservers: vec!["10.233.0.1".into()],
                mac: "02:47:52:00:00:03".into(),
            }),
            relays: vec![GuestRelay {
                name: "tty0".into(),
                port: AGENT_RELAY_PORT_BASE,
                kind: GuestRelayKind::Tcp { listen: 5000 },
            }],
            usb_bus_ids: vec!["1-2".into()],
            program_pty: false,
        }
    }

    #[tokio::test]
    async fn a_run_job_crosses_a_guest_connection() {
        let (worker_end, agent_end) = tokio::io::duplex(64 * 1024);
        let (worker, agent) = tokio::join!(
            GuestConnection::agree(worker_end),
            GuestConnection::agree(agent_end)
        );
        let (mut worker, mut agent) =
            (worker.expect("worker agrees"), agent.expect("agent agrees"));

        let sent = WorkerToAgent::RunJob(Box::new(run_job()));
        worker.send(&sent).await.expect("send");
        let received: Option<WorkerToAgent> = agent.recv().await.expect("recv");

        assert_eq!(received, Some(sent));
    }

    async fn connected(
        worker_range: RangeInclusive<u16>,
    ) -> (
        GuestConnection<tokio::io::DuplexStream>,
        GuestConnection<tokio::io::DuplexStream>,
    ) {
        let (worker_end, agent_end) = tokio::io::duplex(64 * 1024);
        let (worker, agent) = tokio::join!(
            GuestConnection::agree_within(worker_end, &worker_range),
            GuestConnection::agree(agent_end)
        );
        (worker.expect("worker agrees"), agent.expect("agent agrees"))
    }

    #[tokio::test]
    async fn channel_frames_need_the_second_guest_version() {
        let open = WorkerToAgent::OpenChannel {
            channel_id: 3,
            kind: GuestChannelKind::Shell {
                pty: true,
                attach: true,
            },
        };

        let (mut old_worker, mut old_agent) = connected(1..=1).await;
        assert!(old_worker.send(&open).await.is_err());
        old_worker
            .send(&WorkerToAgent::RunJob(Box::new(RunJob {
                program_pty: true,
                ..run_job()
            })))
            .await
            .expect("a run job crosses version 1");
        let Some(WorkerToAgent::RunJob(job)) = old_agent.recv().await.expect("recv") else {
            panic!("expected the run job");
        };
        assert!(!job.program_pty);

        let (mut worker, mut agent) = connected(1..=2).await;
        worker.send(&open).await.expect("send");
        assert_eq!(
            agent.recv::<WorkerToAgent>().await.expect("recv"),
            Some(open)
        );
        let closed = AgentToWorker::CloseChannel {
            channel_id: 3,
            exit_code: Some(130),
            message: None,
        };
        agent.send(&closed).await.expect("send");
        assert_eq!(
            worker.recv::<AgentToWorker>().await.expect("recv"),
            Some(closed)
        );
    }

    #[test]
    fn a_guest_secret_encodes_like_a_name_and_a_byte_vector() {
        let secret = GuestSecret {
            name: "wandb-token".into(),
            value: SecretBytes::from(b"s3cr3t-value".as_slice()),
        };
        let plain = (String::from("wandb-token"), b"s3cr3t-value".to_vec());
        let version = *GUEST_PROTO_VERSIONS.end();

        let encoded = to_bytes(&secret, version).expect("encodes");
        assert_eq!(encoded, to_bytes(&plain, version).expect("encodes"));
        assert_eq!(from_bytes::<GuestSecret>(encoded, version), Ok(secret));
    }

    #[tokio::test]
    async fn an_upload_crosses_a_guest_connection_and_is_settled() {
        let (mut worker, mut agent) = connected(GUEST_PROTO_VERSIONS).await;
        let sent = [
            AgentToWorker::UploadStart {
                upload_id: 1,
                path: "model/w.bin".into(),
                size: 3,
            },
            AgentToWorker::UploadChunk {
                upload_id: 1,
                data: b"abc".to_vec(),
            },
            AgentToWorker::UploadEnd { upload_id: 1 },
        ];
        for msg in &sent {
            agent.send(msg).await.expect("send");
        }
        for msg in sent {
            assert_eq!(
                worker.recv::<AgentToWorker>().await.expect("recv"),
                Some(msg)
            );
        }

        let settled = WorkerToAgent::UploadSettled {
            upload_id: 1,
            error: None,
        };
        worker.send(&settled).await.expect("send");
        assert_eq!(
            agent.recv::<WorkerToAgent>().await.expect("recv"),
            Some(settled)
        );
    }

    #[tokio::test]
    async fn a_frame_of_a_disjoint_version_range_is_refused() {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let mut peer = Framed::new(theirs, LengthDelimitedCodec::new());
        peer.send(version_frame(&(10..=11)))
            .await
            .expect("peer frame");

        assert!(GuestConnection::agree(ours).await.is_err());
    }
}
