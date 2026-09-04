/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! What the graph actor cannot establish itself about an eval batch: which
//! derivations our cache already holds whole, and which an upstream serves.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gradient_core::ServerState;
use gradient_entity::StorePath;
use gradient_graph::UpstreamHit;
use gradient_types::proto::DiscoveredDerivation;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tracing::{error, warn};

const UPSTREAM_WINDOW_MINUTES: i64 = 60;

#[derive(Debug, Default)]
pub struct SubstitutionFacts {
    pub truly_substituted: HashSet<String>,
    pub upstream_substitutable: HashSet<String>,
    pub upstream_hits: HashMap<String, UpstreamHit>,
}

/// The substitution facts the graph actor cannot establish itself: which
/// derivations are already whole in our cache, which an upstream serves, and
/// the narinfo hits to persist. Reads run on the pool; the probe is network.
pub async fn assess_substitutability(
    state: &Arc<ServerState>,
    evaluation: &MEvaluation,
    derivations: &[DiscoveredDerivation],
) -> SubstitutionFacts {
    let mut facts = SubstitutionFacts::default();
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
        return facts;
    }

    let db = &state.worker_db;

    // Whole in our own cache: every output present with a complete closure, so
    // the anchor can be resigned instead of rebuilt.
    let fully_cached: HashSet<String> =
        gradient_db::fetch_in_chunks(&all_hashes, |chunk| async move {
            ECachedPath::find()
                .filter(CCachedPath::Hash.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .unwrap_or_else(|e| {
            error!(error = %e, "substitutability: cached_path lookup failed");
            Vec::new()
        })
        .into_iter()
        .filter(|cp| cp.is_fully_cached() && cp.closure_complete)
        .map(|cp| cp.hash)
        .collect();
    for (drv, hashes) in &outputs_by_drv {
        if !hashes.is_empty() && hashes.iter().all(|h| fully_cached.contains(h)) {
            facts.truly_substituted.insert((*drv).to_owned());
        }
    }

    let Some(project_id) = crate::dispatch::project_id_for_eval(state, evaluation).await else {
        return facts;
    };
    let endpoints =
        gradient_db::upstream_endpoints_for_project(db, project_id, UPSTREAM_WINDOW_MINUTES)
            .await
            .unwrap_or_default();
    if endpoints.is_empty() {
        return facts;
    }

    let known_outputs = gradient_db::fetch_in_chunks(&all_hashes, |chunk| async move {
        EDerivationOutput::find()
            .filter(CDerivationOutput::Hash.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .unwrap_or_else(|e| {
        error!(error = %e, "substitutability: derivation_output lookup failed");
        Vec::new()
    });
    let mut available: HashSet<String> = known_outputs
        .iter()
        .filter(|o| o.external_url.is_some())
        .map(|o| o.hash.clone())
        .collect();
    let cached_anywhere: HashSet<String> = known_outputs
        .iter()
        .filter(|o| o.is_cached_anywhere())
        .map(|o| o.hash.clone())
        .collect();
    let to_probe: Vec<(String, String)> = derivations
        .iter()
        .filter(|d| !facts.truly_substituted.contains(&d.drv_path))
        .flat_map(|d| d.outputs.iter())
        .filter_map(|o| StorePath::parse(&o.path).ok())
        .filter(|sp| !cached_anywhere.contains(sp.hash()))
        .map(|sp| (sp.hash().to_owned(), sp.full()))
        .collect::<HashMap<_, _>>()
        .into_iter()
        .collect();

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
        available.insert(hash.clone());
        facts.upstream_hits.insert(
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

    // A derivation is upstream-substitutable only when every one of its outputs
    // is served, and internal cache presence deliberately does not count: an
    // output whose runtime closure is incomplete would otherwise be flagged,
    // fail substitution, and escalate into a build whose inputs were never
    // produced. The genuinely-whole internal case is `truly_substituted` above.
    for (drv, hashes) in &outputs_by_drv {
        if facts.truly_substituted.contains(*drv) {
            continue;
        }
        if !hashes.is_empty() && hashes.iter().all(|h| available.contains(h)) {
            facts.upstream_substitutable.insert((*drv).to_owned());
        }
    }

    facts
}
