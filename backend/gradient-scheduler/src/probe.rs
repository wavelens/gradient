/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The upstream probe, driven by demand.
//!
//! An output is asked for when its anchor gains demand, never when its batch
//! lands: a hit makes the anchor a relay and demands what its narinfo references,
//! a miss leaves it a builder and demands its build inputs, and the demand each
//! answer moves comes back here as the next round. An evaluation of 44k
//! derivations therefore probes what something actually wants rather than every
//! output it walked.
//!
//! It runs off every graph path because probing is HTTP: a network round trip on
//! the graph actor's transaction would hold the single writer to the graph for its
//! duration.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gradient_core::ServerState;
use gradient_entity::StorePath;
use gradient_types::*;
use gradient_util::supervision::ChildSpec;
use sea_orm::{ColumnTrait, EntityTrait, JoinType, QueryFilter, QuerySelect, RelationTrait};
use tokio::sync::Mutex;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::debug;

pub const HEALTH_NAME: &str = "upstream-probe";
const PROBE_TICK: Duration = Duration::from_secs(1);
const PROBE_BUDGET: Duration = Duration::from_secs(120);
/// How long a probed anchor is remembered. The demand fixpoint reports the same
/// anchor from several directions, and an upstream that answered once will answer
/// the same for a while; the memory is what keeps a round from re-asking.
const PROBE_MEMORY: Duration = Duration::from_secs(300);
/// Outputs asked for in one round. A reply is one narinfo per path, so the bound
/// is about the round trip's tail latency rather than about memory.
const PROBE_BATCH: usize = 256;

/// How often the recovery sweep asks the database for demand whose request never
/// arrived. Only ever reached on an idle tick, and a healthy server answers it
/// with no rows.
const PROBE_SWEEP: Duration = Duration::from_secs(60);
/// How long one pass will keep following the closure down before it hands what is
/// left back to the next tick. Half of [`PROBE_BUDGET`], so the round that carries
/// the pass over it still has the other half to finish in.
const PROBE_DESCENT: Duration = Duration::from_secs(60);

/// The probe as a supervised child of the scheduler. The inbox and the memory
/// outlive a restart, so a crash loses at most the round it was in.
pub fn child_spec(state: &Arc<ServerState>) -> ChildSpec {
    let inbox = Arc::new(Mutex::new(state.probe_requests.take_inbox()));
    let seen: Arc<Mutex<HashMap<DerivationId, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
    let swept = Arc::new(Mutex::new(Instant::now()));
    let state = Arc::clone(state);
    ChildSpec::periodic(HEALTH_NAME, PROBE_TICK, PROBE_BUDGET, move || {
        let state = Arc::clone(&state);
        let inbox = Arc::clone(&inbox);
        let seen = Arc::clone(&seen);
        let swept = Arc::clone(&swept);
        async move {
            probe_pass(&state, &inbox, &seen, &swept)
                .await
                .map_err(|e| gradient_util::supervision::PassError::from(e.to_string()))
        }
    })
}

/// One pass: take what gained demand since the last tick and follow the closure
/// down, a round per level, until it stops or the pass has had its share of the
/// supervision budget.
///
/// The descent is a loop rather than a round a tick because a round's answer
/// demands the next level and hands it straight back: stopping at one would hold
/// every build below it for a tick a level, and a bootstrap chain is dozens of
/// levels deep. It is bounded rather than unbounded because a pass past
/// [`PROBE_BUDGET`] is a supervision failure, and what a bounded pass leaves is
/// picked up by the next tick from the same channel.
async fn probe_pass(
    state: &Arc<ServerState>,
    inbox: &Mutex<Option<UnboundedReceiver<Vec<DerivationId>>>>,
    seen: &Mutex<HashMap<DerivationId, Instant>>,
    swept: &Mutex<Instant>,
) -> Result<()> {
    let started = Instant::now();
    let mut requested = drain_requests(inbox).await;
    if requested.is_empty() && sweep_due(swept).await {
        requested.extend(unanswered_demand(state).await?);
    }

    while !requested.is_empty() {
        let unanswered = probe_round(state, fresh(requested, seen).await).await?;
        forget(seen, &unanswered).await;
        if started.elapsed() > PROBE_DESCENT {
            break;
        }

        requested = drain_requests(inbox).await;
    }

    Ok(())
}

/// Ask for one level and record its answer: the hits go to the graph actor, then
/// the anchors the plan could answer for are marked, which is what lets demand
/// descend past a miss. The recompute that follows reports the next level through
/// the channel. Returns the anchors left unanswered.
async fn probe_round(
    state: &Arc<ServerState>,
    anchors: Vec<DerivationId>,
) -> Result<Vec<DerivationId>> {
    if anchors.is_empty() {
        return Ok(Vec::new());
    }

    let plan = plan_probes(state, &anchors).await?;
    for (evaluation, targets) in plan.rounds {
        for chunk in targets.chunks(PROBE_BATCH) {
            let hits = crate::eval::probe_outputs(state, &evaluation, chunk.to_vec()).await;
            // Logged before the miss returns: a round that asks and finds nothing is
            // the only evidence that the demand ever reached this loop at all.
            debug!(
                hits = hits.len(),
                asked = chunk.len(),
                "upstream probe round"
            );
            if hits.is_empty() {
                continue;
            }
            state
                .graph
                .upstream_hits(hits)
                .await
                .context("apply what the upstream probe found")?;
        }
    }

    // Last, and including what was never asked: an anchor whose every output is
    // already cached has its answer too, and demand stops at an unanswered anchor.
    state
        .graph
        .upstream_probed(plan.answered.clone())
        .await
        .context("record the anchors this round answered for")?;

    let answered: HashSet<DerivationId> = plan.answered.into_iter().collect();
    Ok(anchors
        .into_iter()
        .filter(|a| !answered.contains(a))
        .collect())
}

/// Everything the channel holds: the anchors some commit reported as newly
/// demanded.
async fn drain_requests(
    inbox: &Mutex<Option<UnboundedReceiver<Vec<DerivationId>>>>,
) -> HashSet<DerivationId> {
    let mut requested: HashSet<DerivationId> = HashSet::new();
    if let Some(rx) = inbox.lock().await.as_mut() {
        while let Ok(batch) = rx.try_recv() {
            requested.extend(batch);
        }
    }

    requested
}

/// Whether the recovery sweep is due, taking the slot if it is.
async fn sweep_due(swept: &Mutex<Instant>) -> bool {
    let mut last = swept.lock().await;
    if last.elapsed() < PROBE_SWEEP {
        return false;
    }
    *last = Instant::now();

    true
}

/// Demand whose request never reached this loop. The channel is in memory, so a
/// process that stops between the commit and the send leaves anchors nothing will
/// ever ask about - and demand stops at an unanswered one, so nothing below them
/// would ever be built again. Read on an idle tick and at most once a
/// [`PROBE_SWEEP`]; the two columns are the partial index's, so a healthy server's
/// empty answer is free.
async fn unanswered_demand(state: &Arc<ServerState>) -> Result<Vec<DerivationId>> {
    Ok(unanswered_demand_query()
        .all(&state.worker_db)
        .await
        .context("find demand the upstream probe was never asked about")?
        .into_iter()
        .map(|a| a.derivation)
        .collect())
}

/// Demanded, unanswered and walked. A stub is waiting for the walk, which hands it
/// to the probe when its record lands.
fn unanswered_demand_query() -> sea_orm::Select<EDerivationBuild> {
    EDerivationBuild::find()
        .join(
            JoinType::InnerJoin,
            gradient_entity::derivation_build::Relation::Derivation.def(),
        )
        .filter(CDerivationBuild::Probed.eq(false))
        .filter(CDerivationBuild::Demanded.eq(true))
        .filter(CDerivation::Walked.eq(true))
        .limit(PROBE_BATCH as u64)
}

/// The anchors of `requested` not asked for inside [`PROBE_MEMORY`], recorded as
/// asked. Expiry is folded into the same pass, so the memory is bounded by what
/// gained demand in the last five minutes.
async fn fresh(
    requested: HashSet<DerivationId>,
    seen: &Mutex<HashMap<DerivationId, Instant>>,
) -> Vec<DerivationId> {
    if requested.is_empty() {
        return Vec::new();
    }

    let now = Instant::now();
    let mut seen = seen.lock().await;
    seen.retain(|_, at| now.duration_since(*at) < PROBE_MEMORY);

    let mut fresh: Vec<DerivationId> = requested
        .into_iter()
        .filter(|d| !seen.contains_key(d))
        .collect();
    fresh.sort_unstable();
    for d in &fresh {
        seen.insert(*d, now);
    }

    fresh
}

/// Lets an anchor left unanswered through [`fresh`] again, since the walk resends
/// it once its record lands.
async fn forget(seen: &Mutex<HashMap<DerivationId, Instant>>, unanswered: &[DerivationId]) {
    if unanswered.is_empty() {
        return;
    }

    let mut seen = seen.lock().await;
    for d in unanswered {
        seen.remove(d);
    }
}

#[derive(Debug, Default)]
pub(crate) struct ProbePlan {
    /// What to ask, grouped by an evaluation naming the anchor.
    pub rounds: Vec<(MEvaluation, Vec<(String, String)>)>,
    /// The anchors this round answers for: demanded, with their outputs recorded.
    pub answered: Vec<DerivationId>,
}

/// What one round will ask, grouped by an evaluation that names the anchor: the
/// upstreams are the project's and a project is only reachable through an
/// evaluation. An output already cached anywhere is skipped - we either hold it or
/// already know where it is. An anchor nothing demands, or one the walk has only
/// named and whose outputs are unknown, is neither asked nor answered.
pub(crate) async fn plan_probes(
    state: &Arc<ServerState>,
    anchors: &[DerivationId],
) -> Result<ProbePlan> {
    let db = &state.worker_db;
    let demanded: Vec<DerivationId> = gradient_db::fetch_in_chunks(anchors, |chunk| async move {
        EDerivationBuild::find()
            .filter(CDerivationBuild::Derivation.is_in(chunk))
            .filter(CDerivationBuild::Demanded.eq(true))
            .all(db)
            .await
    })
    .await
    .context("load which requested anchors are demanded")?
    .into_iter()
    .map(|a| a.derivation)
    .collect();
    if demanded.is_empty() {
        return Ok(ProbePlan::default());
    }

    let outputs = gradient_db::fetch_in_chunks(&demanded, |chunk| async move {
        EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("load the outputs of the anchors that gained demand")?;
    let recorded: HashSet<DerivationId> = outputs.iter().map(|o| o.derivation).collect();
    let answered: Vec<DerivationId> = demanded
        .into_iter()
        .filter(|d| recorded.contains(d))
        .collect();

    let jobs = gradient_db::fetch_in_chunks(&answered, |chunk| async move {
        EBuildJob::find()
            .filter(CBuildJob::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("find an evaluation naming each anchor that gained demand")?;
    let mut evaluation_of: HashMap<DerivationId, EvaluationId> = HashMap::new();
    for job in &jobs {
        evaluation_of
            .entry(job.derivation)
            .or_insert(job.evaluation);
    }

    let mut by_evaluation: HashMap<EvaluationId, HashMap<String, String>> = HashMap::new();
    for o in outputs.iter().filter(|o| !o.is_cached_anywhere()) {
        let Some(evaluation) = evaluation_of.get(&o.derivation) else {
            continue;
        };
        let sp = StorePath::from_parts(o.hash.clone(), o.package.clone());
        by_evaluation
            .entry(*evaluation)
            .or_default()
            .insert(o.hash.clone(), sp.full());
    }
    if by_evaluation.is_empty() {
        return Ok(ProbePlan {
            rounds: Vec::new(),
            answered,
        });
    }

    let ids: Vec<EvaluationId> = by_evaluation.keys().copied().collect();
    let evaluations = gradient_db::fetch_in_chunks(&ids, |chunk| async move {
        EEvaluation::find()
            .filter(CEvaluation::Id.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("load the evaluations the probe reads its upstreams through")?;

    let rounds = evaluations
        .into_iter()
        .filter_map(|e| {
            by_evaluation
                .remove(&e.id)
                .map(|targets| (e, targets.into_iter().collect()))
        })
        .collect();

    Ok(ProbePlan { rounds, answered })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::build_job::Model as MBuildJob;
    use gradient_entity::derivation_output::Model as MDerivationOutput;
    use gradient_test_support::prelude::test_state;
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn output(derivation: DerivationId, hash: &str, cached: bool) -> MDerivationOutput {
        MDerivationOutput {
            id: gradient_types::ids::DerivationOutputId::now_v7(),
            derivation,
            name: "out".to_owned(),
            hash: hash.to_owned(),
            package: "hello-2.12".to_owned(),
            is_cached: cached,
            ..Default::default()
        }
    }

    fn demanded(derivation: DerivationId) -> MDerivationBuild {
        MDerivationBuild {
            id: gradient_types::ids::DerivationBuildId::now_v7(),
            derivation,
            demanded: true,
            ..Default::default()
        }
    }

    /// The probe asks for what we do not have. An output already in our cache, or
    /// already resolved on an upstream, costs a narinfo round trip and can only
    /// answer what the row already says.
    #[tokio::test]
    async fn already_cached_outputs_are_not_probed() {
        let derivation = DerivationId::now_v7();
        let evaluation = EvaluationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![demanded(derivation)]])
            .append_query_results([vec![
                output(derivation, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", false),
                output(derivation, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", true),
            ]])
            .append_query_results([vec![MBuildJob {
                id: gradient_types::ids::BuildJobId::now_v7(),
                evaluation,
                derivation,
                ..Default::default()
            }]])
            .append_query_results([vec![MEvaluation {
                id: evaluation,
                ..Default::default()
            }]])
            .into_connection();

        let planned = plan_probes(&test_state(db), &[derivation])
            .await
            .expect("the plan is read")
            .rounds;

        assert_eq!(planned.len(), 1, "{planned:?}");
        let (_, targets) = &planned[0];
        assert_eq!(
            targets.iter().map(|(h, _)| h.as_str()).collect::<Vec<_>>(),
            vec!["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
            "an output already in our cache must not be asked for"
        );
    }

    /// The request channel is in memory, so a process that stops between the commit
    /// and the send leaves demand nothing will ever ask about - and demand stops at
    /// an anchor the probe has not answered, so every build below it would stall
    /// for good. The sweep is the only reader that finds those.
    #[tokio::test]
    async fn the_sweep_finds_demand_whose_request_was_lost() {
        let derivation = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![MDerivationBuild {
                id: gradient_types::ids::DerivationBuildId::now_v7(),
                derivation,
                demanded: true,
                ..Default::default()
            }]])
            .into_connection();

        assert_eq!(
            unanswered_demand(&test_state(db))
                .await
                .expect("the sweep reads"),
            vec![derivation]
        );
    }

    /// The sweep is a database read on a loop that ticks every second. It is the
    /// backstop for a lost request, not the way a round normally starts.
    #[tokio::test]
    async fn the_sweep_runs_at_most_once_an_interval() {
        let swept = Mutex::new(Instant::now());
        assert!(!sweep_due(&swept).await, "a fresh mark is not due");
    }

    /// The pass follows the closure down in a loop, so an empty channel must end it
    /// rather than spin it: this runs every second for the life of the server, and
    /// the mock answers no statement, so any read at all fails here.
    #[tokio::test]
    async fn an_idle_pass_reads_nothing() {
        let state = test_state(MockDatabase::new(DatabaseBackend::Postgres).into_connection());

        probe_pass(
            &state,
            &Mutex::new(None),
            &Mutex::new(HashMap::new()),
            &Mutex::new(Instant::now()),
        )
        .await
        .expect("an idle tick is a no-op");
    }

    /// An anchor no evaluation names has no project, so it has no upstreams to ask:
    /// probing it would be a round trip against every endpoint in the instance.
    #[tokio::test]
    async fn an_unnamed_anchor_is_not_probed() {
        let derivation = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![demanded(derivation)]])
            .append_query_results([vec![output(
                derivation,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                false,
            )]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .into_connection();

        assert!(
            plan_probes(&test_state(db), &[derivation])
                .await
                .expect("the plan is read")
                .rounds
                .is_empty()
        );
    }

    /// A stub the walk has only named has no output rows yet, so nothing was asked
    /// for it. Answering it anyway recorded a miss for every output an upstream
    /// serves, and the build that followed the walk was never undone: the anchor
    /// stays unanswered until its record lands.
    #[tokio::test]
    async fn an_unwalked_anchor_is_left_unanswered() {
        let walked = DerivationId::now_v7();
        let stub = DerivationId::now_v7();
        let evaluation = EvaluationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![demanded(walked), demanded(stub)]])
            .append_query_results([vec![output(
                walked,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                false,
            )]])
            .append_query_results([vec![MBuildJob {
                id: gradient_types::ids::BuildJobId::now_v7(),
                evaluation,
                derivation: walked,
                ..Default::default()
            }]])
            .append_query_results([vec![MEvaluation {
                id: evaluation,
                ..Default::default()
            }]])
            .into_connection();

        let plan = plan_probes(&test_state(db), &[walked, stub])
            .await
            .expect("the plan is read");

        assert_eq!(plan.answered, vec![walked]);
    }

    /// The walk hands every anchor it records to the probe, demanded or not. One
    /// nothing wants is neither asked nor answered: demand arriving later sends it
    /// again, and lazy probing exists to keep the rest off the upstreams.
    #[tokio::test]
    async fn an_undemanded_anchor_is_left_unanswered() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .into_connection();

        let plan = plan_probes(&test_state(db), &[DerivationId::now_v7()])
            .await
            .expect("the plan is read");

        assert!(
            plan.rounds.is_empty() && plan.answered.is_empty(),
            "{plan:?}"
        );
    }

    /// The memory is of answers. An anchor left unanswered is sent again once its
    /// record lands, usually inside the five minutes, and a remembered request
    /// would drop it for good.
    #[tokio::test]
    async fn an_unanswered_anchor_is_asked_again() {
        let seen = Mutex::new(HashMap::new());
        let anchor = DerivationId::now_v7();
        assert_eq!(fresh(HashSet::from([anchor]), &seen).await, vec![anchor]);

        forget(&seen, &[anchor]).await;

        assert_eq!(fresh(HashSet::from([anchor]), &seen).await, vec![anchor]);
    }

    /// Unwalked demand is not lost, it is waiting for the walk. The sweep reads a
    /// page of it at a time, so a page of stubs would hide every walked anchor
    /// behind it.
    #[test]
    fn the_sweep_reads_only_walked_anchors() {
        use sea_orm::QueryTrait as _;
        let sql = unanswered_demand_query()
            .build(DatabaseBackend::Postgres)
            .to_string();
        assert!(sql.contains(r#""derivation"."walked" = TRUE"#), "{sql}");
    }
}
