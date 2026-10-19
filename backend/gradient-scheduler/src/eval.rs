/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use gradient_db::WorkerDb;
use gradient_db::status::EvaluationWarning;
use gradient_graph::UpstreamHit;
use gradient_types::*;
use tokio::sync::Semaphore;
use tracing::warn;

const UPSTREAM_WINDOW_MINUTES: i64 = 60;

#[derive(Debug, Default)]
pub struct ProbeAnswer {
    pub hits: HashMap<String, UpstreamHit>,
    pub unanswered: HashSet<String>,
}

/// This is the only function here leaving the process. Probing is running in its own loop for that
/// reason, never on a graph path.
pub async fn probe_outputs(
    db: &WorkerDb,
    mapping: &Arc<dyn StorePathMapping>,
    upstream_query: &Arc<Semaphore>,
    evaluation: &MEvaluation,
    to_probe: Vec<StorePath>,
) -> ProbeAnswer {
    let mut answer = ProbeAnswer::default();
    if to_probe.is_empty() {
        return answer;
    }

    let Some(project_id) = crate::loops::project_id_for_eval(db, evaluation).await else {
        return answer;
    };
    let endpoints = match gradient_db::caches::upstream::upstream_endpoints_for_project(
        db,
        project_id,
        UPSTREAM_WINDOW_MINUTES,
    )
    .await
    {
        Ok(endpoints) => endpoints,
        Err(e) => {
            warn!(error = %e, evaluation = %evaluation.id, "upstream probe: could not read the upstream caches; asking again later");
            answer.unanswered = to_probe.iter().map(|p| p.hash().to_owned()).collect();
            return answer;
        }
    };
    if endpoints.is_empty() {
        return answer;
    }

    let mapper = match StorePathMapper::for_project(Arc::clone(mapping), project_id).await {
        Ok(mapper) => mapper,
        Err(e) => {
            warn!(error = %e, evaluation = %evaluation.id, "upstream probe: the project's store paths cannot be mapped; treating every path as a miss");
            return answer;
        }
    };
    let id_to_url: HashMap<_, String> = endpoints.iter().map(|e| (e.id, e.url.clone())).collect();
    let batch = gradient_core::upstream::probe_batch(
        endpoints,
        Arc::clone(upstream_query),
        mapper,
        to_probe,
    )
    .await;
    warn_unanswered(db, evaluation, &batch.unanswered, &id_to_url).await;

    // The same URL under different upstream ids is folding into one metric series (#417).
    let mut by_url: HashMap<String, gradient_db::caches::upstream::UpstreamAccum> = HashMap::new();
    for (id, accum) in &batch.stats {
        if let Some(url) = id_to_url.get(id) {
            by_url.entry(url.clone()).or_default().merge(accum);
        }
    }

    let bucket = {
        use chrono::Timelike as _;
        let now = gradient_types::now();
        now.with_second(0)
            .and_then(|t: chrono::NaiveDateTime| t.with_nanosecond(0))
            .unwrap_or(now)
    };
    if let Err(e) =
        gradient_db::caches::upstream::upsert_upstream_metrics(db, bucket, &by_url).await
    {
        warn!(error = %e, "failed to flush upstream metrics");
    }

    answer.unanswered = batch.unanswered.into_keys().collect();
    for (hash, cp) in batch.found {
        answer.hits.insert(
            hash,
            UpstreamHit {
                url: cp.url.clone(),
                nar_hash: cp.nar_hash.clone(),
                file_hash: cp.file_hash.clone(),
                file_size: cp.file_size.map(|v| v as i64),
                nar_size: cp.nar_size.map(|v| v as i64),
                references: cp.references.as_ref().map(|r| r.join(" ")),
                deriver: cp.deriver.clone(),
                ca: cp.ca.clone(),
            },
        );
    }

    answer
}

async fn warn_unanswered(
    db: &WorkerDb,
    evaluation: &MEvaluation,
    unanswered: &HashMap<String, Vec<CacheUpstreamId>>,
    id_to_url: &HashMap<CacheUpstreamId, String>,
) {
    let urls: BTreeSet<&str> = unanswered
        .values()
        .flatten()
        .filter_map(|id| id_to_url.get(id))
        .map(String::as_str)
        .collect();

    for url in urls {
        EvaluationWarning::UpstreamUnanswered { url }
            .report(db, evaluation.id)
            .await;
    }
}
