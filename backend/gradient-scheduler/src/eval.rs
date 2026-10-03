/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gradient_core::ServerState;
use gradient_entity::StorePath;
use gradient_graph::UpstreamHit;
use gradient_types::*;
use gradient_wire::types::DiscoveredDerivation;
use tracing::{error, warn};

const UPSTREAM_WINDOW_MINUTES: i64 = 60;

#[tracing::instrument(level = "debug", skip_all, fields(derivations = derivations.len()))]
pub async fn assess_cached(
    state: &Arc<ServerState>,
    derivations: &[DiscoveredDerivation],
) -> HashSet<String> {
    let mut truly_substituted = HashSet::new();
    let outputs_by_drv: HashMap<&str, Vec<String>> = derivations
        .iter()
        .map(|d| {
            let hashes = d
                .outputs
                .iter()
                .filter_map(|o| StorePath::parse(&o.path).ok())
                .map(|sp| sp.hash().to_owned())
                .collect();
            (d.drv_path.as_str(), hashes)
        })
        .collect();
    let all_hashes: Vec<String> = outputs_by_drv
        .values()
        .flatten()
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if all_hashes.is_empty() {
        return truly_substituted;
    }

    let db = &state.worker_db;

    let fully_cached: HashSet<String> =
        gradient_db::fetch_in_chunks(&all_hashes, |chunk| async move {
            gradient_db::graph::runtime_can_start::complete_output_hashes(db, &chunk).await
        })
        .await
        .unwrap_or_else(|e| {
            error!(error = %e, "cache availability: complete-output lookup failed");
            Vec::new()
        })
        .into_iter()
        .collect();
    for (drv, hashes) in &outputs_by_drv {
        if !hashes.is_empty() && hashes.iter().all(|h| fully_cached.contains(h)) {
            truly_substituted.insert((*drv).to_owned());
        }
    }

    truly_substituted
}

#[derive(Debug, Default)]
pub struct ProbeAnswer {
    pub hits: HashMap<String, UpstreamHit>,
    pub unanswered: HashSet<String>,
}

/// This is the only function here leaving the process. Probing is running in its own loop for that
/// reason, never on a graph path.
pub async fn probe_outputs(
    state: &Arc<ServerState>,
    evaluation: &MEvaluation,
    to_probe: Vec<(String, String)>,
) -> ProbeAnswer {
    let mut answer = ProbeAnswer::default();
    if to_probe.is_empty() {
        return answer;
    }

    let db = &state.worker_db;
    let Some(project_id) = crate::loops::project_id_for_eval(state, evaluation).await else {
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
            answer.unanswered = to_probe.into_iter().map(|(hash, _)| hash).collect();
            return answer;
        }
    };
    if endpoints.is_empty() {
        return answer;
    }

    let id_to_url: HashMap<_, String> = endpoints.iter().map(|e| (e.id, e.url.clone())).collect();
    let batch = gradient_core::upstream::probe_batch(
        endpoints,
        Arc::clone(&state.upstream_query),
        to_probe,
    )
    .await;
    warn_unanswered(state, evaluation, &batch.unanswered, &id_to_url).await;

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
    state: &Arc<ServerState>,
    evaluation: &MEvaluation,
    unanswered: &HashMap<String, Vec<CacheUpstreamId>>,
    id_to_url: &HashMap<CacheUpstreamId, String>,
) {
    let mut outputs_by_url: HashMap<&str, usize> = HashMap::new();
    for url in unanswered
        .values()
        .flatten()
        .filter_map(|id| id_to_url.get(id))
    {
        *outputs_by_url.entry(url.as_str()).or_default() += 1;
    }

    for (url, outputs) in outputs_by_url {
        warn!(upstream = url, outputs, evaluation = %evaluation.id, "upstream cache did not answer; asking again later instead of building");
        let message = format!(
            "upstream cache {url} did not answer; outputs it may serve wait for it instead of being built. Deactivate the upstream to build them"
        );
        if let Err(e) = gradient_db::status::insert_evaluation_message_once(
            &state.worker_db,
            evaluation.id,
            MessageLevel::Warning,
            message,
            Some(crate::probe::HEALTH_NAME.to_owned()),
        )
        .await
        {
            warn!(error = %e, upstream = url, "failed to record the unanswered upstream warning");
        }
    }
}
