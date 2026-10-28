/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;

use gradient_db::exec_jobs::{attempts, nodes};
use gradient_entity::exec_job::ExecJobStatus;
use gradient_pool::guest_channels::{GuestChannelEvent, GuestChannelHandle};
use gradient_types::MExecJob;
use gradient_types::exec::ExecSpec;
use gradient_types::ids::ExecJobId;
use gradient_wire::types::GuestChannelKind;
use tokio::sync::mpsc;

use crate::Scheduler;
use crate::jobs::exec_job_key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestChannelRefusal {
    NotRunning,
    SshDisabled,
    NoProgramTerminal,
    PortNotDeclared(u16),
    NoSuchNode(u32),
    WorkerOffline,
}

impl fmt::Display for GuestChannelRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning => f.write_str("the exec job is not running"),
            Self::SshDisabled => f.write_str("the exec job was started with ssh = false"),
            Self::NoProgramTerminal => f.write_str("program has no terminal, set ssh = true"),
            Self::PortNotDeclared(port) => write!(f, "port {port} is not in the job's ports"),
            Self::NoSuchNode(node) => write!(f, "no node {node} in this job"),
            Self::WorkerOffline => f.write_str("the job's worker is not connected"),
        }
    }
}

impl std::error::Error for GuestChannelRefusal {}

#[derive(Debug)]
pub struct GuestChannelOpened {
    pub handle: GuestChannelHandle,
    pub events: mpsc::Receiver<GuestChannelEvent>,
}

pub(crate) fn check_guest_channel(
    status: ExecJobStatus,
    spec_ssh: Option<bool>,
    ports: &[u16],
    kind: &GuestChannelKind,
) -> Result<(), GuestChannelRefusal> {
    match kind {
        GuestChannelKind::Console => check_console(status),
        GuestChannelKind::Shell { attach, .. } => check_shell(status, spec_ssh, *attach),
        GuestChannelKind::Forward { port } => check_forward(status, spec_ssh, ports, *port),
    }
}

fn check_console(status: ExecJobStatus) -> Result<(), GuestChannelRefusal> {
    match status {
        ExecJobStatus::Starting | ExecJobStatus::Running => Ok(()),
        _ => Err(GuestChannelRefusal::NotRunning),
    }
}

fn check_shell(
    status: ExecJobStatus,
    spec_ssh: Option<bool>,
    attach: bool,
) -> Result<(), GuestChannelRefusal> {
    if status != ExecJobStatus::Running {
        return Err(GuestChannelRefusal::NotRunning);
    }
    match (spec_ssh, attach) {
        (Some(false), true) => Err(GuestChannelRefusal::NoProgramTerminal),
        (Some(false), false) => Err(GuestChannelRefusal::SshDisabled),
        _ => Ok(()),
    }
}

fn check_forward(
    status: ExecJobStatus,
    spec_ssh: Option<bool>,
    ports: &[u16],
    port: u16,
) -> Result<(), GuestChannelRefusal> {
    check_shell(status, spec_ssh, false)?;
    if !ports.contains(&port) {
        return Err(GuestChannelRefusal::PortNotDeclared(port));
    }
    Ok(())
}

fn spec_of(job: &MExecJob) -> ExecSpec {
    serde_json::from_value(job.spec.clone()).unwrap_or_default()
}

impl Scheduler {
    pub async fn open_guest_channel(
        &self,
        job: ExecJobId,
        node: u32,
        kind: GuestChannelKind,
    ) -> anyhow::Result<Result<GuestChannelOpened, GuestChannelRefusal>> {
        let db = &self.db.worker_db;
        let Some(row) = nodes::node_of(db, job, node).await? else {
            return Ok(Err(GuestChannelRefusal::NoSuchNode(node)));
        };
        let spec = spec_of(&row);
        if let Err(refusal) = check_guest_channel(row.status, spec.ssh, &spec.ports, &kind) {
            return Ok(Err(refusal));
        }
        let Some(attempt) = attempts::open_of_job(db, row.id).await? else {
            return Ok(Err(GuestChannelRefusal::NotRunning));
        };
        let Some(port) = self.worker_port(&attempt.worker).await else {
            return Ok(Err(GuestChannelRefusal::WorkerOffline));
        };
        let (handle, events) =
            self.guest_channels
                .open(&attempt.worker, port, exec_job_key(row.id), kind);
        Ok(Ok(GuestChannelOpened { handle, events }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shells_and_forwards_need_a_running_job_with_ssh_and_a_declared_port() {
        use ExecJobStatus::*;
        let shell = GuestChannelKind::Shell {
            pty: true,
            attach: false,
        };
        let attach = GuestChannelKind::Shell {
            pty: true,
            attach: true,
        };
        assert_eq!(check_guest_channel(Running, None, &[], &shell), Ok(()));
        assert_eq!(
            check_guest_channel(Starting, None, &[], &shell),
            Err(GuestChannelRefusal::NotRunning)
        );
        assert_eq!(
            check_guest_channel(Running, Some(false), &[], &shell),
            Err(GuestChannelRefusal::SshDisabled)
        );
        assert_eq!(
            check_guest_channel(Running, Some(false), &[], &attach),
            Err(GuestChannelRefusal::NoProgramTerminal)
        );
        assert_eq!(
            check_guest_channel(
                Running,
                None,
                &[6006],
                &GuestChannelKind::Forward { port: 8080 }
            ),
            Err(GuestChannelRefusal::PortNotDeclared(8080))
        );
    }

    #[test]
    fn the_console_opens_while_the_guest_boots_and_without_ssh() {
        assert_eq!(
            check_guest_channel(
                ExecJobStatus::Starting,
                Some(false),
                &[],
                &GuestChannelKind::Console
            ),
            Ok(())
        );
        assert_eq!(
            check_guest_channel(
                ExecJobStatus::Completed,
                None,
                &[],
                &GuestChannelKind::Console
            ),
            Err(GuestChannelRefusal::NotRunning)
        );
    }

    #[test]
    fn refusals_read_as_the_user_sees_them() {
        assert_eq!(
            GuestChannelRefusal::PortNotDeclared(22).to_string(),
            "port 22 is not in the job's ports"
        );
        assert_eq!(
            GuestChannelRefusal::NoSuchNode(2).to_string(),
            "no node 2 in this job"
        );
        assert_eq!(
            GuestChannelRefusal::NoProgramTerminal.to_string(),
            "program has no terminal, set ssh = true"
        );
    }
}
