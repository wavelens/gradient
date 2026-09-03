/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The sessions supervisor: one linked `SessionActor` per connection, drained
//! before the tree stops them, and re-registered when the scheduler core is
//! respawned.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwapOption;
use gradient_scheduler::Scheduler;
use gradient_scheduler::actor::{CALL_TIMEOUT, SessionSignal};
use gradient_util::shutdown::Shutdown;
use gradient_util::supervision::{ChildCtx, ChildSpec};
use ractor::rpc::CallResult;
use ractor::{
    Actor, ActorId, ActorProcessingErr, ActorRef, ActorStatus, RpcReplyPort, SupervisionEvent,
};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use super::session_actor::{SESSION_DRAIN_BUDGET, SessionActor, SessionArgs, SessionMsg};

pub type AttachedSession = (ActorRef<SessionMsg>, JoinHandle<()>);

pub enum SessionsMsg {
    Attach(SessionArgs, RpcReplyPort<Result<AttachedSession, String>>),
    Reattach,
}

/// The process-wide handle the upgrade paths use to attach a connection; the
/// factory republishes the supervisor's ref on every (re)spawn.
pub struct SessionsHandle {
    actor: ArcSwapOption<ActorRef<SessionsMsg>>,
}

impl SessionsHandle {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            actor: ArcSwapOption::empty(),
        })
    }

    pub fn child_spec(self: &Arc<Self>, scheduler: Arc<Scheduler>) -> ChildSpec {
        let handle = Arc::clone(self);
        ChildSpec::Custom {
            name: "sessions",
            spawn: Arc::new(move |ctx: ChildCtx| {
                let handle = Arc::clone(&handle);
                let scheduler = Arc::clone(&scheduler);
                Box::pin(async move {
                    let args = SessionsArgs {
                        shutdown: scheduler.state.shutdown.clone(),
                        scheduler,
                    };
                    let (actor, _) = Actor::spawn_linked(None, Sessions, args, ctx.parent).await?;
                    handle.actor.store(Some(Arc::new(actor.clone())));
                    Ok(actor.get_cell())
                })
            }),
        }
    }

    pub async fn attach(&self, args: SessionArgs) -> Result<AttachedSession, String> {
        let Some(actor) = self.actor.load_full() else {
            return Err("sessions supervisor not running".into());
        };

        match actor
            .call(|reply| SessionsMsg::Attach(args, reply), Some(CALL_TIMEOUT))
            .await
        {
            Ok(CallResult::Success(result)) => result,
            Ok(CallResult::Timeout) => Err("sessions supervisor timed out".into()),
            Ok(CallResult::SenderError) => Err("sessions supervisor dropped the reply".into()),
            Err(e) => Err(e.to_string()),
        }
    }
}

pub struct Sessions;

pub struct SessionsArgs {
    pub scheduler: Arc<Scheduler>,
    pub shutdown: Shutdown,
}

pub struct SessionsState {
    scheduler: Arc<Scheduler>,
    live: HashMap<ActorId, (String, ActorRef<SessionMsg>)>,
    core_watch: JoinHandle<()>,
}

impl Actor for Sessions {
    type Msg = SessionsMsg;
    type State = SessionsState;
    type Arguments = SessionsArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let mut cores = args.scheduler.core_changes();
        let core_watch = args.shutdown.spawn(async move {
            while cores.changed().await.is_ok() {
                if cores.borrow_and_update().is_some()
                    && myself.send_message(SessionsMsg::Reattach).is_err()
                {
                    return;
                }
            }
        });

        Ok(SessionsState {
            scheduler: args.scheduler,
            live: HashMap::new(),
            core_watch,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        st: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SessionsMsg::Attach(args, reply) => {
                let peer_id = args.peer_id.clone();
                let result =
                    match Actor::spawn_linked(None, SessionActor, args, myself.get_cell()).await {
                        Ok((actor, join)) => {
                            st.live.insert(actor.get_id(), (peer_id, actor.clone()));
                            Ok((actor, join))
                        }
                        Err(e) => Err(e.to_string()),
                    };
                let _ = reply.send(result);
            }
            SessionsMsg::Reattach => {
                info!(
                    sessions = st.live.len(),
                    "scheduler core republished; re-registering sessions"
                );
                for (_, actor) in st.live.values() {
                    let _ = actor.send_message(SessionMsg::Reattach);
                }
            }
        }

        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        evt: SupervisionEvent,
        st: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let (cell, failed) = match evt {
            SupervisionEvent::ActorFailed(cell, err) => {
                warn!(error = %err, "session actor panicked");
                (cell, true)
            }
            SupervisionEvent::ActorTerminated(cell, _, _) => (cell, false),
            _ => return Ok(()),
        };
        if let Some((peer_id, _)) = st.live.remove(&cell.get_id())
            && failed
        {
            st.scheduler.unregister_worker(&peer_id).await;
        }

        Ok(())
    }

    /// Sessions get `Draining` and their budget before the tree stops them.
    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        st: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        st.core_watch.abort();
        info!(sessions = st.live.len(), "draining sessions");
        for (_, actor) in st.live.values() {
            let _ = actor.send_message(SessionMsg::Signal(SessionSignal::Drain));
        }
        let deadline = Instant::now() + SESSION_DRAIN_BUDGET + Duration::from_secs(1);
        while Instant::now() < deadline
            && st
                .live
                .values()
                .any(|(_, a)| a.get_cell().get_status() != ActorStatus::Stopped)
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        for (_, actor) in st.live.values() {
            actor.stop(Some("shutdown".into()));
        }

        Ok(())
    }
}
