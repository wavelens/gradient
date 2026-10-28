/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::gateway_user::GatewayTarget;
use crate::session::Session;
use gradient_db::exec_jobs::reads;
use gradient_db::lookup::by_id;
use gradient_pool::guest_channels::GuestChannelEvent;
use gradient_scheduler::GuestChannelOpened;
use gradient_types::permissions::Permission;
use gradient_types::*;
use gradient_wire::types::GuestChannelKind;
use russh::server::{ChannelOpenHandle, Msg};
use russh::{Channel, ChannelMsg, ChannelOpenFailure};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;
use tokio::sync::watch;

const STDERR: u32 = 1;
const NO_EXIT_CODE: u32 = 255;
const OPEN_FAILED: &str = "the terminal could not be opened";
const NOT_ALLOWED: &str = "this login needs the TriggerEvaluation permission on the project";

pub type TerminalSize = watch::Receiver<(u16, u16)>;

fn unknown_job(job: ExecJobId) -> String {
    format!("exec job {job} does not exist in this project")
}

fn no_running_job(task: &str) -> String {
    format!("task {task} has no running exec job")
}

pub async fn resolve_target(
    session: &Session,
    target: &GatewayTarget,
) -> Result<(ExecJobId, u32), String> {
    if !session.may(Permission::TriggerEvaluation) {
        return Err(NOT_ALLOWED.into());
    }
    let resolved = match target {
        GatewayTarget::Job { job, node } => job_in_project(session, *job)
            .await
            .map(|found| found.then_some((*job, *node))),
        GatewayTarget::Task(task) => running_job_of_task(session, task)
            .await
            .map(|job| job.map(|job| (job, 0))),
    };
    match (resolved, target) {
        (Ok(Some(found)), _) => Ok(found),
        (Ok(None), GatewayTarget::Job { job, .. }) => Err(unknown_job(*job)),
        (Ok(None), GatewayTarget::Task(task)) => Err(no_running_job(task)),
        (Err(e), _) => {
            tracing::warn!(error = %e, "resolving an exec job login failed");
            Err(OPEN_FAILED.into())
        }
    }
}

async fn job_in_project(session: &Session, job: ExecJobId) -> Result<bool, sea_orm::DbErr> {
    let db = &session.handles.db.web_db;
    let Some(job) = by_id::<EExecJob, _>(db, job).await? else {
        return Ok(false);
    };
    let Some(task) = by_id::<EEvaluation, _>(db, job.evaluation)
        .await?
        .and_then(|evaluation| evaluation.task)
    else {
        return Ok(false);
    };
    let project = by_id::<ETask, _>(db, task).await?.map(|task| task.project);
    Ok(project == Some(session.project.id))
}

async fn running_job_of_task(
    session: &Session,
    task: &str,
) -> Result<Option<ExecJobId>, sea_orm::DbErr> {
    let db = &session.handles.db.web_db;
    let Some(task) = ETask::find()
        .filter(CTask::Project.eq(session.project.id))
        .filter(CTask::Name.eq(task))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    reads::latest_running_of_task(db, task.id).await
}

async fn open_channel(
    session: &Session,
    target: &GatewayTarget,
    kind: GuestChannelKind,
) -> Result<GuestChannelOpened, String> {
    let (job, node) = resolve_target(session, target).await?;
    match session
        .handles
        .scheduler
        .open_guest_channel(job, node, kind)
        .await
    {
        Ok(Ok(opened)) => Ok(opened),
        Ok(Err(refusal)) => Err(refusal.to_string()),
        Err(e) => {
            tracing::warn!(exec_job = %job, error = %e, "opening a guest channel failed");
            Err(OPEN_FAILED.into())
        }
    }
}

pub async fn run_shell(
    mut channel: Channel<Msg>,
    session: Arc<Session>,
    target: GatewayTarget,
    kind: GuestChannelKind,
    sizes: Option<TerminalSize>,
) {
    let closed = match open_channel(&session, &target, kind).await {
        Ok(opened) => pump(&mut channel, opened, sizes, &session).await,
        Err(message) => Some(GuestChannelEvent::Closed {
            exit_code: Some(1),
            message: Some(message),
        }),
    };
    if let Some(GuestChannelEvent::Closed { exit_code, message }) = closed {
        finish_shell(&channel, exit_code, message).await;
    }
}

async fn finish_shell(channel: &Channel<Msg>, exit_code: Option<i32>, message: Option<String>) {
    if let Some(message) = message {
        let _ = channel
            .extended_data_bytes(STDERR, format!("{message}\r\n"))
            .await;
    }
    let status = exit_code
        .and_then(|code| u32::try_from(code).ok())
        .unwrap_or(NO_EXIT_CODE);
    let _ = channel.exit_status(status).await;
    let _ = channel.eof().await;
    let _ = channel.close().await;
}

pub async fn run_forward(
    mut channel: Channel<Msg>,
    reply: ChannelOpenHandle,
    session: Arc<Session>,
    target: GatewayTarget,
    port: u16,
) {
    let opened = match open_channel(&session, &target, GuestChannelKind::Forward { port }).await {
        Ok(opened) => opened,
        Err(message) => {
            tracing::debug!(%port, %message, "refused a port forward");
            reply.reject(ChannelOpenFailure::ConnectFailed).await;
            return;
        }
    };
    reply.accept().await;
    if pump(&mut channel, opened, None, &session).await.is_some() {
        let _ = channel.eof().await;
        let _ = channel.close().await;
    }
}

pub async fn pump(
    channel: &mut Channel<Msg>,
    opened: GuestChannelOpened,
    mut sizes: Option<TerminalSize>,
    session: &Session,
) -> Option<GuestChannelEvent> {
    let GuestChannelOpened { handle, mut events } = opened;
    if let Some(sizes) = &sizes {
        let (cols, rows) = *sizes.borrow();
        handle.resize(cols, rows);
    }
    loop {
        tokio::select! {
            () = session.closed.cancelled() => return None,
            message = channel.wait() => match message {
                Some(ChannelMsg::Data { data }) => handle.send(&data),
                Some(ChannelMsg::Close) | None => return None,
                Some(_) => {}
            },
            Some(size) = next_size(&mut sizes) => handle.resize(size.0, size.1),
            event = events.recv() => match event {
                Some(GuestChannelEvent::Data(data)) => {
                    if channel.data_bytes(data).await.is_err() {
                        return None;
                    }
                }
                Some(closed) => return Some(closed),
                None => return Some(GuestChannelEvent::Closed { exit_code: None, message: None }),
            },
        }
    }
}

async fn next_size(sizes: &mut Option<TerminalSize>) -> Option<(u16, u16)> {
    let receiver = sizes.as_mut()?;
    if receiver.changed().await.is_err() {
        *sizes = None;
        return None;
    }
    Some(*receiver.borrow_and_update())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SshHandles;
    use gradient_fixtures::mock::mock_db;
    use gradient_fixtures::rows::{project, user};
    use gradient_test_support::state::TestState;
    use gradient_types::permissions::mask_from;
    use sea_orm::MockDatabase;
    use uuid::Uuid;

    fn job_id() -> ExecJobId {
        ExecJobId::new(Uuid::from_u128(0x7a))
    }

    async fn session(db: MockDatabase, permissions: Vec<Permission>) -> Session {
        let state = TestState::new().web_db(db.into_connection()).build();
        Session::open(
            SshHandles::for_tests(&state),
            user(),
            project(),
            mask_from(&permissions),
            vec![],
        )
        .await
        .expect("session")
    }

    fn job_target() -> GatewayTarget {
        GatewayTarget::Job {
            job: job_id(),
            node: 0,
        }
    }

    #[tokio::test]
    async fn a_job_of_another_project_is_not_reachable_through_this_login() {
        let other_task = MTask {
            id: TaskId::new(Uuid::from_u128(0xb7)),
            project: ProjectId::new(Uuid::from_u128(0xb0)),
            ..Default::default()
        };
        let evaluation = MEvaluation {
            task: Some(other_task.id),
            ..Default::default()
        };
        let job = MExecJob {
            id: job_id(),
            evaluation: evaluation.id,
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![job]])
            .append_query_results([vec![evaluation]])
            .append_query_results([vec![other_task]]);
        let elsewhere = resolve_target(
            &session(db, vec![Permission::TriggerEvaluation]).await,
            &job_target(),
        )
        .await;

        let unknown = mock_db().append_query_results([Vec::<MExecJob>::new()]);
        let missing = resolve_target(
            &session(unknown, vec![Permission::TriggerEvaluation]).await,
            &job_target(),
        )
        .await;

        assert_eq!(elsewhere, Err(unknown_job(job_id())));
        assert_eq!(elsewhere, missing);
    }

    #[tokio::test]
    async fn a_login_without_task_write_is_refused() {
        let refused = resolve_target(
            &session(mock_db(), vec![Permission::ViewProject]).await,
            &job_target(),
        )
        .await;
        assert_eq!(refused, Err(NOT_ALLOWED.into()));
    }

    #[tokio::test]
    async fn a_task_without_a_running_job_is_named() {
        let task = MTask {
            name: "nightly".into(),
            project: project().id,
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![task]])
            .append_query_results([Vec::<MExecJob>::new()]);
        let refused = resolve_target(
            &session(db, vec![Permission::TriggerEvaluation]).await,
            &GatewayTarget::Task("nightly".into()),
        )
        .await;
        assert_eq!(refused, Err("task nightly has no running exec job".into()));
    }
}
