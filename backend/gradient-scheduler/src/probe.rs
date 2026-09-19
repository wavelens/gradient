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
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
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

/// The probe as a supervised child of the scheduler. The inbox and the memory
/// outlive a restart, so a crash loses at most the round it was in.
pub fn child_spec(state: &Arc<ServerState>) -> ChildSpec {
    let inbox = Arc::new(Mutex::new(state.probe_requests.take_inbox()));
    let seen: Arc<Mutex<HashMap<DerivationId, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
    let state = Arc::clone(state);
    ChildSpec::periodic(HEALTH_NAME, PROBE_TICK, PROBE_BUDGET, move || {
        let state = Arc::clone(&state);
        let inbox = Arc::clone(&inbox);
        let seen = Arc::clone(&seen);
        async move {
            probe_pass(&state, &inbox, &seen)
                .await
                .map_err(|e| gradient_util::supervision::PassError::from(e.to_string()))
        }
    })
}

/// One round: take what gained demand since the last tick, drop what was asked for
/// recently, and ask the upstreams of the project each anchor's evaluation belongs
/// to. The hits go back to the graph actor, whose own recompute reports the next
/// round through the same channel.
async fn probe_pass(
    state: &Arc<ServerState>,
    inbox: &Mutex<Option<UnboundedReceiver<Vec<DerivationId>>>>,
    seen: &Mutex<HashMap<DerivationId, Instant>>,
) -> Result<()> {
    let anchors = fresh_anchors(inbox, seen).await;
    if anchors.is_empty() {
        return Ok(());
    }

    for (evaluation, targets) in plan_probes(state, &anchors).await? {
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

    Ok(())
}

/// Drain the channel and return the anchors not asked for inside [`PROBE_MEMORY`],
/// recording them as asked. Expiry is folded into the same pass, so the memory is
/// bounded by what gained demand in the last five minutes.
async fn fresh_anchors(
    inbox: &Mutex<Option<UnboundedReceiver<Vec<DerivationId>>>>,
    seen: &Mutex<HashMap<DerivationId, Instant>>,
) -> Vec<DerivationId> {
    let mut requested: HashSet<DerivationId> = HashSet::new();
    if let Some(rx) = inbox.lock().await.as_mut() {
        while let Ok(batch) = rx.try_recv() {
            requested.extend(batch);
        }
    }
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

/// What one round will ask, grouped by an evaluation that names the anchor: the
/// upstreams are the project's and a project is only reachable through an
/// evaluation. An output already cached anywhere is skipped - we either hold it or
/// already know where it is.
pub(crate) async fn plan_probes(
    state: &Arc<ServerState>,
    anchors: &[DerivationId],
) -> Result<Vec<(MEvaluation, Vec<(String, String)>)>> {
    let db = &state.worker_db;
    let outputs = gradient_db::fetch_in_chunks(anchors, |chunk| async move {
        EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("load the outputs of the anchors that gained demand")?;

    let jobs = gradient_db::fetch_in_chunks(anchors, |chunk| async move {
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
        return Ok(Vec::new());
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

    Ok(evaluations
        .into_iter()
        .filter_map(|e| {
            by_evaluation
                .remove(&e.id)
                .map(|targets| (e, targets.into_iter().collect()))
        })
        .collect())
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

    /// The probe asks for what we do not have. An output already in our cache, or
    /// already resolved on an upstream, costs a narinfo round trip and can only
    /// answer what the row already says.
    #[tokio::test]
    async fn already_cached_outputs_are_not_probed() {
        let derivation = DerivationId::now_v7();
        let evaluation = EvaluationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
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
            .expect("the plan is read");

        assert_eq!(planned.len(), 1, "{planned:?}");
        let (_, targets) = &planned[0];
        assert_eq!(
            targets.iter().map(|(h, _)| h.as_str()).collect::<Vec<_>>(),
            vec!["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
            "an output already in our cache must not be asked for"
        );
    }

    /// An anchor no evaluation names has no project, so it has no upstreams to ask:
    /// probing it would be a round trip against every endpoint in the instance.
    #[tokio::test]
    async fn an_unnamed_anchor_is_not_probed() {
        let derivation = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
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
                .is_empty()
        );
    }
}
