/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The one place a durable side effect leaves the process. A state change owes
//! the outside world an `outbox` row, written in the transaction that made the
//! change; this crate claims those rows, expands each event into the deliveries
//! it implies, and runs them on a bounded pool with retries. Nothing here is
//! reachable from `gradient-db`, which is what keeps the persistence layer from
//! knowing that forges exist.

pub mod actor;
pub mod consume;
pub mod deliver;

use std::sync::Arc;
use std::time::Duration;

use gradient_core::ServerState;
use gradient_util::supervision::{ChildCtx, ChildSpec};
use ractor::factory::queues::DefaultQueue;
use ractor::factory::routing::QueuerRouting;
use ractor::factory::{DiscardSettings, Factory, FactoryArguments, worker::worker_builder};
use ractor::{Actor, ActorCell, SpawnErr};
use tracing::warn;

use actor::{EffectsActor, EffectsArgs, EffectsMsg, HEALTH_NAME};
use deliver::{DbStore, DeliverJob, Deliverer, EffectsCtx, FactoryDispatch};

/// Concurrent deliveries. The bound the process-wide action semaphore used to
/// carry, now a worker count: a mass status wave fires one event per anchor and
/// unbounded execution exhausted the DB pool.
pub const EFFECTS_WORKERS: usize = 8;
/// The backstop pass. A lost wake costs this much latency and never a delivery.
pub const TICK: Duration = Duration::from_secs(30);

type EffectsFactory = Factory<
    gradient_types::ids::OutboxId,
    DeliverJob,
    (),
    Deliverer,
    QueuerRouting<gradient_types::ids::OutboxId, DeliverJob>,
    DefaultQueue<gradient_types::ids::OutboxId, DeliverJob>,
>;

/// The supervised child: the worker factory, the claiming actor above it, and
/// the task that turns `DbContext::outbox_wake` into a coalesced `Wake`.
pub fn child_spec(state: Arc<ServerState>) -> ChildSpec {
    ChildSpec::Custom {
        name: HEALTH_NAME,
        stop_last: false,
        spawn: Arc::new(move |child: ChildCtx| {
            let state = Arc::clone(&state);
            Box::pin(async move { spawn(state, child).await })
        }),
    }
}

async fn spawn(state: Arc<ServerState>, child: ChildCtx) -> Result<ActorCell, SpawnErr> {
    let ctx = EffectsCtx::new(Arc::clone(&state));
    let (factory, _) = Actor::spawn_linked(
        Some("effects-workers".to_owned()),
        EffectsFactory::default(),
        FactoryArguments {
            worker_builder: Box::new(worker_builder(move |_wid| {
                (
                    Deliverer {
                        ctx: Arc::clone(&ctx),
                    },
                    (),
                )
            })),
            num_initial_workers: EFFECTS_WORKERS,
            router: QueuerRouting::default(),
            queue: DefaultQueue::default(),
            discard_handler: None,
            discard_settings: DiscardSettings::None,
            dead_mans_switch: None,
            capacity_controller: None,
            lifecycle_hooks: None,
            stats: None,
        },
        child.parent.clone(),
    )
    .await?;

    child.health.register(HEALTH_NAME);
    let (effects, _) = Actor::spawn_linked(
        None,
        EffectsActor::<DbStore, FactoryDispatch>::default(),
        EffectsArgs {
            store: DbStore::new(state.worker_db.clone()),
            dispatch: Arc::new(FactoryDispatch { factory }),
            capacity: EFFECTS_WORKERS,
            tick: TICK,
            health: Some(Arc::clone(&child.health)),
        },
        child.parent,
    )
    .await?;

    // The wake is a `Notify`, so a burst of writers collapses into one
    // notification here and the actor collapses the rest.
    let waker = effects.clone();
    let shutdown = state.shutdown.clone();
    shutdown.spawn(async move {
        loop {
            tokio::select! {
                () = child.cancel.cancelled() => break,
                () = state.outbox_wake.notified() => {
                    if waker.cast(EffectsMsg::Wake).is_err() {
                        warn!("the effects actor is gone; stopping the outbox waker");
                        break;
                    }
                }
            }
        }
    });

    Ok(effects.get_cell())
}
