/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use anyhow::{Context, Result, anyhow};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::codec::agreement::{agree, version_frame};
use crate::codec::{Proto, from_bytes, to_bytes};

pub const GUEST_PROTO_VERSIONS: RangeInclusive<u16> = 1..=1;
pub const AGENT_VSOCK_PORT: u32 = 7000;
pub const AGENT_RELAY_PORT_BASE: u32 = 7100;

const MAX_GUEST_FRAME: usize = 16 * 1024 * 1024;

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum WorkerToAgent {
    Attach { output_offset: u64 },
    RunJob(Box<RunJob>),
    Kill,
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
    pub value: Vec<u8>,
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
    Ready { agent_version: String },
    ProgramStarted,
    Output { data: Vec<u8> },
    OutputDropped { bytes: u64 },
    Phase { name: String, payload: Vec<u8> },
    SetupFailed { message: String },
    Exited { code: i32 },
    JobLost,
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
        let codec = LengthDelimitedCodec::builder()
            .max_frame_length(MAX_GUEST_FRAME)
            .new_codec();
        let mut frames = Framed::new(stream, codec);
        frames
            .send(version_frame(&GUEST_PROTO_VERSIONS))
            .await
            .context("sending the guest protocol range")?;
        let peer = frames
            .next()
            .await
            .ok_or_else(|| anyhow!("guest peer closed before the version agreement"))?
            .context("reading the guest protocol range")?;
        let version = agree(&GUEST_PROTO_VERSIONS, &peer).map_err(|e| anyhow!(e))?;
        Ok(Self { frames, version })
    }

    pub async fn send<T: Proto>(&mut self, msg: &T) -> Result<()> {
        let bytes = to_bytes(msg, self.version)?;
        self.frames.send(bytes).await?;
        Ok(())
    }

    pub async fn recv<T: Proto>(&mut self) -> Result<Option<T>> {
        match self.frames.next().await {
            None => Ok(None),
            Some(frame) => Ok(Some(from_bytes(frame?.freeze(), self.version)?)),
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
                value: b"s3cr3t".to_vec(),
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
            }),
            relays: vec![GuestRelay {
                name: "tty0".into(),
                port: AGENT_RELAY_PORT_BASE,
                kind: GuestRelayKind::Tcp { listen: 5000 },
            }],
            usb_bus_ids: vec!["1-2".into()],
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

    #[tokio::test]
    async fn a_frame_of_a_disjoint_version_range_is_refused() {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let mut peer = Framed::new(theirs, LengthDelimitedCodec::new());
        peer.send(version_frame(&(2..=3)))
            .await
            .expect("peer frame");

        assert!(GuestConnection::agree(ours).await.is_err());
    }
}
