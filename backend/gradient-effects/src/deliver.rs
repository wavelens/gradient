/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The two ends the actor is generic over, wired to what actually exists: the
//! outbox table behind [`DbStore`], and a ractor factory of [`Deliverer`]
//! workers behind [`FactoryDispatch`]. A worker that panics mid-delivery is the
//! factory's to replace; its row stays leased until `next_attempt_at`.

use std::sync::Arc;

use gradient_ci::CiContext;
use gradient_core::ServerState;
use gradient_db::outbox::{OutboxRow, Outcome};
use gradient_db::{DbContext, WorkerDb};
use gradient_types::ids::OutboxId;
use ractor::factory::{FactoryMessage, Job, JobOptions, Worker, WorkerId};
use ractor::{ActorProcessingErr, ActorRef};

use crate::actor::{Dispatch, EffectsMsg, OutboxStore};
use crate::consume::consume;

/// What a delivery may reach. Held by every worker, so it is a handle bundle
/// and never a connection.
pub struct EffectsCtx {
    state: Arc<ServerState>,
}

impl EffectsCtx {
    pub fn new(state: Arc<ServerState>) -> Arc<Self> {
        Arc::new(Self { state })
    }

    pub fn ci(&self) -> CiContext {
        self.state.ci()
    }

    pub fn db(&self) -> DbContext {
        self.state.db()
    }
}

/// One row handed to one worker, with the actor to answer.
pub type DeliverJob = (OutboxRow, ActorRef<EffectsMsg>);

pub struct DbStore {
    db: WorkerDb,
}

impl DbStore {
    pub fn new(db: WorkerDb) -> Arc<Self> {
        Arc::new(Self { db })
    }
}

impl OutboxStore for DbStore {
    async fn claim_due(&self, limit: usize) -> anyhow::Result<Vec<OutboxRow>> {
        Ok(gradient_db::outbox::claim_due(&self.db, limit).await?)
    }

    async fn mark(&self, row: &OutboxRow, outcome: &Outcome) -> anyhow::Result<()> {
        Ok(gradient_db::outbox::mark(&self.db, row, outcome).await?)
    }
}

pub struct Deliverer {
    pub ctx: Arc<EffectsCtx>,
}

impl Worker for Deliverer {
    type Key = OutboxId;
    type Message = DeliverJob;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _wid: WorkerId,
        _factory: &ActorRef<FactoryMessage<Self::Key, Self::Message>>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(args)
    }

    async fn handle(
        &self,
        _wid: WorkerId,
        _factory: &ActorRef<FactoryMessage<Self::Key, Self::Message>>,
        Job {
            key,
            msg: (row, reply),
            ..
        }: Job<Self::Key, Self::Message>,
        _state: &mut Self::State,
    ) -> Result<Self::Key, ActorProcessingErr> {
        let outcome = consume(&self.ctx, &row).await;
        let _ = reply.cast(EffectsMsg::Done { row, outcome });

        Ok(key)
    }
}

pub struct FactoryDispatch {
    pub factory: ActorRef<FactoryMessage<OutboxId, DeliverJob>>,
}

impl Dispatch for FactoryDispatch {
    fn dispatch(&self, row: OutboxRow, reply: ActorRef<EffectsMsg>) {
        let _ = self.factory.cast(FactoryMessage::Dispatch(Job {
            key: row.id,
            msg: (row, reply),
            options: JobOptions::default(),
            accepted: None,
        }));
    }
}
