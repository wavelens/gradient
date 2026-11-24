/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A worker panicking mid-delivery is replaced by the factory. Its row is leased until
//! `next_attempt_at`.

use std::sync::Arc;

use gradient_ci::CiContext;
use gradient_db::deliveries::pending::{Outcome, PendingDelivery};
use gradient_db::{DbContext, WorkerDb};
use gradient_scheduler::Scheduler;
use gradient_types::ids::PendingDeliveryId;
use ractor::factory::{FactoryMessage, Job, JobOptions, Worker, WorkerId};
use ractor::{ActorProcessingErr, ActorRef};

use crate::actor::{EffectsMsg, Handoff, PendingDeliveryStore};
use crate::consume::{
    ConsumedDeliveryWork, consume_delivery_job_with_row_outcomes,
    split_claimed_rows_into_delivery_jobs,
};

pub struct EffectsCtx {
    ci: CiContext,
    scheduler: Arc<Scheduler>,
}

impl EffectsCtx {
    pub fn new(ci: CiContext, scheduler: Arc<Scheduler>) -> Arc<Self> {
        Arc::new(Self { ci, scheduler })
    }

    pub fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    pub fn ci(&self) -> CiContext {
        self.ci.clone()
    }

    pub fn db(&self) -> DbContext {
        self.ci.db.clone()
    }
}

pub type DeliverJob = (ConsumedDeliveryWork, ActorRef<EffectsMsg>);

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

    async fn mark(&self, settled: &[(PendingDelivery, Outcome)]) -> anyhow::Result<()> {
        Ok(gradient_db::deliveries::pending::mark(&self.db, settled).await?)
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
            msg: (job, reply),
            ..
        }: Job<Self::Key, Self::Message>,
        _state: &mut Self::State,
    ) -> Result<Self::Key, ActorProcessingErr> {
        let finished_rows_with_outcomes =
            consume_delivery_job_with_row_outcomes(&self.ctx, job).await;
        let _ = reply.cast(EffectsMsg::Done {
            finished_rows_with_outcomes,
        });

        Ok(key)
    }
}

pub struct FactoryHandoff {
    pub factory: ActorRef<FactoryMessage<PendingDeliveryId, DeliverJob>>,
}

impl Handoff for FactoryHandoff {
    fn hand_off(&self, rows: Vec<PendingDelivery>, reply: ActorRef<EffectsMsg>) {
        for job in split_claimed_rows_into_delivery_jobs(rows) {
            let _ = self.factory.cast(FactoryMessage::Dispatch(Job {
                key: job.first_pending_delivery_id(),
                msg: (job, reply.clone()),
                options: JobOptions::default(),
                accepted: None,
            }));
        }
    }
}
