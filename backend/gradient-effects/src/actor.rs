/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gradient_db::deliveries::pending::{Outcome, PendingDelivery};
use gradient_types::ids::PendingDeliveryId;
use gradient_util::supervision::SupervisorHealth;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use tracing::warn;

pub const HEALTH_NAME: &str = "effects";

pub trait PendingDeliveryStore: Send + Sync + 'static {
    fn claim_due(
        &self,
        limit: usize,
    ) -> impl Future<Output = anyhow::Result<Vec<PendingDelivery>>> + Send;
    fn mark(
        &self,
        row: &PendingDelivery,
        outcome: &Outcome,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}

pub trait Handoff: Send + Sync + 'static {
    fn hand_off(&self, row: PendingDelivery, reply: ActorRef<EffectsMsg>);
}

pub enum EffectsMsg {
    Wake,
    Pass,
    Tick,
    Done {
        row: PendingDelivery,
        outcome: Outcome,
    },
}

pub struct EffectsArgs<S, D> {
    pub store: Arc<S>,
    pub hand_off: Arc<D>,
    pub capacity: usize,
    pub tick: Duration,
    pub health: Option<Arc<SupervisorHealth>>,
}

pub struct EffectsState<S, D> {
    args: EffectsArgs<S, D>,
    in_flight: HashSet<PendingDeliveryId>,
    pass_scheduled: bool,
}

pub struct EffectsActor<S, D>(PhantomData<fn() -> (S, D)>);

impl<S, D> Default for EffectsActor<S, D> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<S: PendingDeliveryStore, D: Handoff> Actor for EffectsActor<S, D> {
    type Msg = EffectsMsg;
    type State = EffectsState<S, D>;
    type Arguments = EffectsArgs<S, D>;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        myself.send_after(args.tick, || EffectsMsg::Tick);
        let _ = myself.send_message(EffectsMsg::Pass);

        Ok(EffectsState {
            args,
            in_flight: HashSet::new(),
            pass_scheduled: true,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        st: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            // The pass is sent to the back of the mailbox, behind every queued wake. A burst of
            // notifying writers is folded into one claim this way.
            EffectsMsg::Wake => {
                if !st.pass_scheduled {
                    st.pass_scheduled = true;
                    let _ = myself.send_message(EffectsMsg::Pass);
                }
            }
            EffectsMsg::Pass => {
                st.pass_scheduled = false;
                pass(&myself, st).await;
            }
            EffectsMsg::Tick => {
                pass(&myself, st).await;
                myself.send_after(st.args.tick, || EffectsMsg::Tick);
            }
            EffectsMsg::Done { row, outcome } => {
                st.in_flight.remove(&row.id);
                if let Err(e) = st.args.store.mark(&row, &outcome).await {
                    warn!(error = %e, delivery = %row.id, "failed to record a delivery outcome");
                }
                pass(&myself, st).await;
            }
        }

        Ok(())
    }
}

async fn pass<S: PendingDeliveryStore, D: Handoff>(
    myself: &ActorRef<EffectsMsg>,
    st: &mut EffectsState<S, D>,
) {
    loop {
        let free = st.args.capacity.saturating_sub(st.in_flight.len());
        if free == 0 {
            break;
        }

        let rows = match st.args.store.claim_due(free).await {
            Ok(rows) => rows,
            Err(e) => {
                record(&st.args.health, Err(&e));
                warn!(error = %e, "failed to claim due pending deliveries");
                return;
            }
        };

        let claimed = rows.len();
        for row in rows {
            st.in_flight.insert(row.id);
            st.args.hand_off.hand_off(row, myself.clone());
        }

        if claimed < free {
            break;
        }
    }

    record(&st.args.health, Ok(()));
}

fn record(health: &Option<Arc<SupervisorHealth>>, outcome: Result<(), &anyhow::Error>) {
    let Some(health) = health else {
        return;
    };
    health.with(HEALTH_NAME, |h| match outcome {
        Ok(()) => h.last_ok_at = Some(Instant::now()),
        Err(e) => {
            h.pass_errors += 1;
            h.last_error = Some(e.to_string());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::pending_delivery::PendingDeliveryKind;
    use gradient_util::sync::Mutex;
    use std::collections::VecDeque;

    struct Store {
        rows: Mutex<VecDeque<PendingDelivery>>,
        claims: Mutex<Vec<usize>>,
        marks: Mutex<Vec<(PendingDeliveryId, Outcome)>>,
    }

    impl Store {
        fn with(rows: impl IntoIterator<Item = PendingDelivery>) -> Arc<Self> {
            Arc::new(Self {
                rows: Mutex::new(rows.into_iter().collect()),
                claims: Mutex::new(Vec::new()),
                marks: Mutex::new(Vec::new()),
            })
        }
    }

    impl PendingDeliveryStore for Store {
        async fn claim_due(&self, limit: usize) -> anyhow::Result<Vec<PendingDelivery>> {
            self.claims.lock().push(limit);
            let mut rows = self.rows.lock();
            Ok((0..limit).filter_map(|_| rows.pop_front()).collect())
        }

        async fn mark(&self, row: &PendingDelivery, outcome: &Outcome) -> anyhow::Result<()> {
            self.marks.lock().push((row.id, outcome.clone()));
            Ok(())
        }
    }

    #[derive(Default)]
    struct Held(Mutex<Vec<(PendingDelivery, ActorRef<EffectsMsg>)>>);

    impl Handoff for Held {
        fn hand_off(&self, row: PendingDelivery, reply: ActorRef<EffectsMsg>) {
            self.0.lock().push((row, reply));
        }
    }

    fn row(n: u8) -> PendingDelivery {
        PendingDelivery {
            id: PendingDeliveryId::now_v7(),
            kind: PendingDeliveryKind::ActionDelivery,
            key: n.to_string(),
            payload: serde_json::json!({}),
            attempts: 0,
        }
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    async fn spawn(store: Arc<Store>, held: Arc<Held>, capacity: usize) -> ActorRef<EffectsMsg> {
        let (actor, _) = Actor::spawn(
            None,
            EffectsActor::<Store, Held>::default(),
            EffectsArgs {
                store,
                hand_off: held,
                capacity,
                tick: Duration::from_secs(3600),
                health: None,
            },
        )
        .await
        .expect("the effects actor starts");

        actor
    }

    #[tokio::test]
    async fn in_flight_never_exceeds_the_worker_count() {
        let store = Store::with((0..20).map(row));
        let held = Arc::new(Held::default());
        let actor = spawn(Arc::clone(&store), Arc::clone(&held), 3).await;

        actor.cast(EffectsMsg::Wake).unwrap();
        actor.cast(EffectsMsg::Wake).unwrap();
        settle().await;

        assert_eq!(held.0.lock().len(), 3);
        assert_eq!(*store.claims.lock(), vec![3], "two wakes, one claim");

        let (done, reply) = held.0.lock().remove(0);
        reply
            .cast(EffectsMsg::Done {
                row: done,
                outcome: Outcome::Delivered,
            })
            .unwrap();
        settle().await;

        assert_eq!(held.0.lock().len(), 3, "one slot freed, one row claimed");
        assert_eq!(*store.claims.lock(), vec![3, 1]);
        assert!(matches!(
            store.marks.lock().as_slice(),
            [(_, Outcome::Delivered)]
        ));

        actor.stop_and_wait(None, None).await.unwrap();
    }

    #[tokio::test]
    async fn a_full_pool_claims_nothing() {
        let store = Store::with((0..8).map(row));
        let held = Arc::new(Held::default());
        let actor = spawn(Arc::clone(&store), Arc::clone(&held), 2).await;

        actor.cast(EffectsMsg::Wake).unwrap();
        settle().await;
        actor.cast(EffectsMsg::Wake).unwrap();
        settle().await;

        assert_eq!(held.0.lock().len(), 2);
        assert_eq!(
            *store.claims.lock(),
            vec![2],
            "the second wake found no room"
        );

        actor.stop_and_wait(None, None).await.unwrap();
    }

    #[tokio::test]
    async fn a_burst_of_wakes_runs_exactly_one_pass() {
        let store = Store::with([]);
        let held = Arc::new(Held::default());
        let actor = spawn(Arc::clone(&store), Arc::clone(&held), 2).await;

        for _ in 0..10 {
            actor.cast(EffectsMsg::Wake).unwrap();
        }
        settle().await;

        assert_eq!(
            *store.claims.lock(),
            vec![2, 2],
            "the boot pass, then one claim for the whole burst"
        );

        actor.stop_and_wait(None, None).await.unwrap();
    }

    #[tokio::test]
    async fn a_failed_delivery_is_marked_and_its_slot_freed() {
        let store = Store::with([row(1)]);
        let held = Arc::new(Held::default());
        let actor = spawn(Arc::clone(&store), Arc::clone(&held), 2).await;

        actor.cast(EffectsMsg::Wake).unwrap();
        settle().await;

        let (failed, reply) = held.0.lock().remove(0);
        reply
            .cast(EffectsMsg::Done {
                row: failed,
                outcome: Outcome::Retry("502".into()),
            })
            .unwrap();
        settle().await;

        assert!(matches!(
            store.marks.lock().as_slice(),
            [(_, Outcome::Retry(e))] if e.as_str() == "502"
        ));

        actor.stop_and_wait(None, None).await.unwrap();
    }
}
