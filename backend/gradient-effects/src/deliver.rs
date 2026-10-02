/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A worker panicking mid-delivery is replaced by the factory. Its row is leased until
//! `next_attempt_at`.

use std::sync::Arc;

use gradient_ci::CiContext;
use gradient_core::ServerState;
use gradient_db::deliveries::pending::{Outcome, PendingDelivery};
use gradient_db::{DbContext, WorkerDb};
use gradient_types::ids::PendingDeliveryId;
use ractor::factory::{FactoryMessage, Job, JobOptions, Worker, WorkerId};
use ractor::{ActorProcessingErr, ActorRef};

use crate::actor::{EffectsMsg, Handoff, PendingDeliveryStore};
use crate::consume::consume;

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

pub type DeliverJob = (PendingDelivery, ActorRef<EffectsMsg>);

pub struct DbStore {
    db: WorkerDb,
}

impl DbStore {
    pub fn new(db: WorkerDb) -> Arc<Self> {
        Arc::new(Self { db })
    }
}

impl PendingDeliveryStore for DbStore {
    async fn claim_due(&self, limit: usize) -> anyhow::Result<Vec<PendingDelivery>> {
        Ok(gradient_db::deliveries::pending::claim_due(&self.db, limit).await?)
    }

    async fn mark(&self, row: &PendingDelivery, outcome: &Outcome) -> anyhow::Result<()> {
        Ok(gradient_db::deliveries::pending::mark(&self.db, row, outcome).await?)
    }
}

pub struct Deliverer {
    pub ctx: Arc<EffectsCtx>,
}

impl Worker for Deliverer {
    type Key = PendingDeliveryId;
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

pub struct FactoryHandoff {
    pub factory: ActorRef<FactoryMessage<PendingDeliveryId, DeliverJob>>,
}

impl Handoff for FactoryHandoff {
    fn hand_off(&self, row: PendingDelivery, reply: ActorRef<EffectsMsg>) {
        let _ = self.factory.cast(FactoryMessage::Dispatch(Job {
            key: row.id,
            msg: (row, reply),
            options: JobOptions::default(),
            accepted: None,
        }));
    }
}
