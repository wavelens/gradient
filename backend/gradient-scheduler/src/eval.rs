/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! What the graph actor cannot establish itself: which derivations our cache
//! already holds whole (asked once per eval batch), and which an upstream serves
//! (asked by the probe loop, for the anchors demand turns on).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gradient_core::ServerState;
use gradient_entity::StorePath;
use gradient_graph::UpstreamHit;
use gradient_types::*;
use gradient_wire::types::DiscoveredDerivation;
use tracing::{error, warn};

const UPSTREAM_WINDOW_MINUTES: i64 = 60;

/// The drv paths of `derivations` whose every output is already whole in our own
/// cache, so the anchor can be resigned instead of rebuilt. A pure read: the
/// upstream question is [`probe_outputs`]'s and runs only once something wants
/// the anchor.
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

    // Whole in our own cache: every output present and its producing anchor whole,
    // so the anchor can be resigned instead of rebuilt.
    let fully_cached: HashSet<String> =
        gradient_db::fetch_in_chunks(&all_hashes, |chunk| async move {
            gradient_db::whole_output_hashes(db, &chunk).await
        })
        .await
        .unwrap_or_else(|e| {
            error!(error = %e, "substitutability: whole-output lookup failed");
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

/// Ask the upstreams of `evaluation`'s project for `to_probe`, folding the
/// per-endpoint metrics the round produced. Network: the only function here that
/// leaves the process, and the reason probing runs on its own loop rather than on
/// a graph path.
pub async fn probe_outputs(
    state: &Arc<ServerState>,
    evaluation: &MEvaluation,
    to_probe: Vec<(String, String)>,
) -> HashMap<String, UpstreamHit> {
    let mut hits = HashMap::new();
    if to_probe.is_empty() {
        return hits;
    }

    let db = &state.worker_db;
    let Some(project_id) = crate::dispatch::project_id_for_eval(state, evaluation).await else {
        return hits;
    };
    let endpoints =
        gradient_db::upstream_endpoints_for_project(db, project_id, UPSTREAM_WINDOW_MINUTES)
            .await
            .unwrap_or_default();
    if endpoints.is_empty() {
        return hits;
    }

    let id_to_url: HashMap<_, String> = endpoints.iter().map(|e| (e.id, e.url.clone())).collect();
    let (found, stats) = gradient_core::upstream::probe_batch(
        gradient_util::http::download_client().clone(),
        endpoints,
        Arc::clone(&state.upstream_query),
        to_probe,
    )
    .await;

    // Same URL under different upstream ids folds into one metric series (#417).
    let mut by_url: HashMap<String, gradient_db::UpstreamAccum> = HashMap::new();
    for (id, accum) in &stats {
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
    if let Err(e) = gradient_db::upsert_upstream_metrics(db, bucket, &by_url).await {
        warn!(error = %e, "failed to flush upstream metrics");
    }

    for (hash, cp) in found {
        hits.insert(
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

    hits
}
