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
        settled: &[(PendingDelivery, Outcome)],
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}

pub trait Handoff: Send + Sync + 'static {
    fn hand_off(&self, rows: Vec<PendingDelivery>, reply: ActorRef<EffectsMsg>);
}

pub enum EffectsMsg {
    Wake,
    Pass,
    Tick,
    Done {
        finished_rows_with_outcomes: Vec<(PendingDelivery, Outcome)>,
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
    delivery_outcomes_waiting_for_database_write: Vec<(PendingDelivery, Outcome)>,
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
            delivery_outcomes_waiting_for_database_write: Vec::new(),
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
            EffectsMsg::Wake => schedule_pass(&myself, st),
            EffectsMsg::Pass => {
                st.pass_scheduled = false;
                pass(&myself, st).await;
            }
            EffectsMsg::Tick => {
                pass(&myself, st).await;
                myself.send_after(st.args.tick, || EffectsMsg::Tick);
            }
            EffectsMsg::Done {
                finished_rows_with_outcomes,
            } => {
                for (row, _) in &finished_rows_with_outcomes {
                    st.in_flight.remove(&row.id);
                }
                st.delivery_outcomes_waiting_for_database_write
                    .extend(finished_rows_with_outcomes);
                schedule_pass(&myself, st);
            }
        }

        Ok(())
    }
}

fn schedule_pass<S, D>(myself: &ActorRef<EffectsMsg>, st: &mut EffectsState<S, D>) {
    if !st.pass_scheduled {
        st.pass_scheduled = true;
        let _ = myself.send_message(EffectsMsg::Pass);
    }
}

/// A pass can first write all outcomes from jobs that completed since the previous pass in one statement. The pass can claim new rows after that. Outcomes from jobs that finished together go into one write.
async fn pass<S: PendingDeliveryStore, D: Handoff>(
    myself: &ActorRef<EffectsMsg>,
    st: &mut EffectsState<S, D>,
) {
    let settled = std::mem::take(&mut st.delivery_outcomes_waiting_for_database_write);
    if let Err(e) = st.args.store.mark(&settled).await {
        warn!(error = %e, rows = settled.len(), "Failed to record the delivery outcome.");
    }

    loop {
        let free = st.args.capacity.saturating_sub(st.in_flight.len());
        if free == 0 {
            break;
        }

        let mut rows = match st.args.store.claim_due(free).await {
            Ok(rows) => rows,
            Err(e) => {
                record(&st.args.health, Err(&e));
                warn!(error = %e, "failed to claim due pending deliveries");
                return;
            }
        };

        let claimed = rows.len();
        rows.retain(|row| st.in_flight.insert(row.id));
        if !rows.is_empty() {
            st.args.hand_off.hand_off(rows, myself.clone());
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
        marks: Mutex<Vec<Vec<(PendingDeliveryId, Outcome)>>>,
    }

    impl Store {
        fn with(rows: impl IntoIterator<Item = PendingDelivery>) -> Arc<Self> {
            Arc::new(Self {
                rows: Mutex::new(rows.into_iter().collect()),
                claims: Mutex::new(Vec::new()),
                marks: Mutex::new(Vec::new()),
            })
        }

        fn outcomes(&self) -> Vec<Vec<Outcome>> {
            self.marks
                .lock()
                .iter()
                .map(|set| set.iter().map(|(_, outcome)| outcome.clone()).collect())
                .collect()
        }
    }

    impl PendingDeliveryStore for Store {
        async fn claim_due(&self, limit: usize) -> anyhow::Result<Vec<PendingDelivery>> {
            self.claims.lock().push(limit);
            let mut rows = self.rows.lock();
            Ok((0..limit).filter_map(|_| rows.pop_front()).collect())
        }

        async fn mark(&self, settled: &[(PendingDelivery, Outcome)]) -> anyhow::Result<()> {
            if !settled.is_empty() {
                self.marks.lock().push(
                    settled
                        .iter()
                        .map(|(row, outcome)| (row.id, outcome.clone()))
                        .collect(),
                );
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct Held(Mutex<Vec<(PendingDelivery, ActorRef<EffectsMsg>)>>);

    impl Handoff for Held {
        fn hand_off(&self, rows: Vec<PendingDelivery>, reply: ActorRef<EffectsMsg>) {
            self.0
                .lock()
                .extend(rows.into_iter().map(|row| (row, reply.clone())));
        }
    }

    fn row(n: u8) -> PendingDelivery {
        PendingDelivery {
            id: PendingDeliveryId::now_v7(),
            kind: PendingDeliveryKind::ActionDelivery,
            key: n.to_string(),
            payload: serde_json::json!({}),
            attempts: 0,
            revision: 0,
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

    fn finish(held: &Held, outcome: Outcome) {
        let (row, reply) = held.0.lock().remove(0);
        reply
            .cast(EffectsMsg::Done {
                finished_rows_with_outcomes: vec![(row, outcome)],
            })
            .unwrap();
    }

    #[tokio::test]
    async fn in_flight_never_exceeds_the_capacity() {
        let store = Store::with((0..20).map(row));
        let held = Arc::new(Held::default());
        let actor = spawn(Arc::clone(&store), Arc::clone(&held), 3).await;

        actor.cast(EffectsMsg::Wake).unwrap();
        actor.cast(EffectsMsg::Wake).unwrap();
        settle().await;

        assert_eq!(held.0.lock().len(), 3);
        assert_eq!(*store.claims.lock(), vec![3], "two wakes, one claim");

        finish(&held, Outcome::Delivered);
        settle().await;

        assert_eq!(
            held.0.lock().len(),
            3,
            "One slot freed up. The actor claimed one row."
        );
        assert_eq!(*store.claims.lock(), vec![3, 1]);
        assert_eq!(store.outcomes(), vec![vec![Outcome::Delivered]]);

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
    async fn a_burst_of_finished_jobs_is_marked_in_one_write() {
        let store = Store::with((0..4).map(row));
        let held = Arc::new(Held::default());
        let actor = spawn(Arc::clone(&store), Arc::clone(&held), 4).await;
        settle().await;

        let finished: Vec<_> = held.0.lock().drain(..).collect();
        let reply = finished[0].1.clone();
        for (row, _) in finished {
            reply
                .cast(EffectsMsg::Done {
                    finished_rows_with_outcomes: vec![(row, Outcome::Delivered)],
                })
                .unwrap();
        }
        settle().await;

        assert_eq!(
            store.outcomes(),
            vec![vec![Outcome::Delivered; 4]],
            "Four completed jobs, one write."
        );

        actor.stop_and_wait(None, None).await.unwrap();
    }

    #[tokio::test]
    async fn a_pending_delivery_still_being_worked_on_is_not_handed_to_a_second_worker_job_after_its_lease_expires()
     {
        let first = row(1);
        let store = Store::with([first.clone()]);
        let held = Arc::new(Held::default());
        let actor = spawn(Arc::clone(&store), Arc::clone(&held), 4).await;
        settle().await;

        store.rows.lock().push_back(first.clone());
        actor.cast(EffectsMsg::Wake).unwrap();
        settle().await;

        assert_eq!(
            held.0
                .lock()
                .iter()
                .map(|(row, _)| row.id)
                .collect::<Vec<_>>(),
            vec![first.id]
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

        finish(&held, Outcome::Retry("502".into()));
        settle().await;

        assert_eq!(store.outcomes(), vec![vec![Outcome::Retry("502".into())]]);

        actor.stop_and_wait(None, None).await.unwrap();
    }
}
