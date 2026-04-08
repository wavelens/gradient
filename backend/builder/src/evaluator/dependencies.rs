/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Evaluation-time dependency graph construction on top of the split
//! `derivation` / `build` schema.
//!
//! The BFS walks derivations (not builds). For each derivation we:
//!   1. Find-or-create the `derivation` row for `(organization, drv_path)`.
//!   2. If newly created: upsert its `derivation_output` rows, record the
//!      edges into `derivation_dependency`, and enqueue its references for
//!      further traversal.
//!   3. If already existed and has `derivation_dependency` edges populated:
//!      skip traversing its subgraph (the DB already holds it) — just walk
//!      the in-DB closure and materialise build rows for every member.
//!   4. Create a fresh `build` row for this evaluation. Status is
//!      `Substituted` if the derivation is already in the Nix store,
//!      `Created` otherwise (promoted to `Queued` by the caller once all
//!      rows are persisted).

use anyhow::{Context, Result};
use chrono::Utc;
use entity::build::BuildStatus;
use gradient_core::executer::*;
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tracing::{debug, trace};
use uuid::Uuid;

/// Accumulates newly-created rows across derivation processing. Everything
/// is bulk-inserted by the caller once evaluation completes so scheduling
/// cannot race against partial state.
pub(super) struct EvaluationAccumulator {
    /// Builds created for this evaluation. One per derivation in every
    /// reached closure; inserted regardless of whether the derivation is
    /// new or reused.
    pub(super) builds: Vec<MBuild>,
    /// Newly-created derivations (deferred insert).
    pub(super) new_derivations: Vec<MDerivation>,
    /// Newly-created derivation_output rows (deferred insert).
    pub(super) new_derivation_outputs: Vec<ADerivationOutput>,
    /// Newly-created derivation_dependency edges (deferred insert).
    pub(super) new_derivation_dependencies: Vec<MDerivationDependency>,
    /// Entry-point builds (top-level wildcard matches).
    pub(super) entry_point_build_ids: Vec<(Uuid, String)>,
    /// Features to attach after derivations are inserted.
    /// Key is derivation_id.
    pub(super) pending_features: Vec<(Uuid, Vec<String>)>,
}

impl EvaluationAccumulator {
    pub(super) fn new() -> Self {
        Self {
            builds: vec![],
            new_derivations: vec![],
            new_derivation_outputs: vec![],
            new_derivation_dependencies: vec![],
            entry_point_build_ids: vec![],
            pending_features: vec![],
        }
    }
}

/// Looks up a derivation by `(organization, path)`.
pub(super) async fn find_derivation(
    state: &Arc<ServerState>,
    organization_id: Uuid,
    drv_path: &str,
) -> Result<Option<MDerivation>> {
    EDerivation::find()
        .filter(CDerivation::Organization.eq(organization_id))
        .filter(CDerivation::DerivationPath.eq(drv_path))
        .one(&state.db)
        .await
        .context("Failed to query derivation")
}

/// Creates a build row for an existing derivation in this evaluation. The
/// build's status depends on whether the derivation is already present in
/// the Nix store (`Substituted`) or needs to be built (`Created`).
fn make_build_row(evaluation_id: Uuid, derivation_id: Uuid, in_store: bool) -> MBuild {
    let now = Utc::now().naive_utc();
    let id = Uuid::new_v4();
    MBuild {
        id,
        evaluation: evaluation_id,
        derivation: derivation_id,
        status: if in_store {
            BuildStatus::Substituted
        } else {
            BuildStatus::Created
        },
        server: None,
        log_id: if in_store { None } else { Some(id) },
        build_time_ms: None,
        created_at: now,
        updated_at: now,
    }
}

/// Walks the in-DB dependency closure of `derivation_id` and returns every
/// derivation reachable (inclusive of the root).
async fn load_closure(
    state: &Arc<ServerState>,
    root_derivation_id: Uuid,
) -> Result<Vec<MDerivation>> {
    let mut seen: HashSet<Uuid> = HashSet::new();
    let mut out: Vec<MDerivation> = Vec::new();
    let mut queue: VecDeque<Uuid> = VecDeque::new();
    queue.push_back(root_derivation_id);
    seen.insert(root_derivation_id);

    while let Some(id) = queue.pop_front() {
        if let Some(d) = EDerivation::find_by_id(id)
            .one(&state.db)
            .await
            .context("load_closure: fetch derivation")?
        {
            out.push(d);
        }

        let edges = EDerivationDependency::find()
            .filter(CDerivationDependency::Derivation.eq(id))
            .all(&state.db)
            .await
            .context("load_closure: fetch dependency edges")?;
        for e in edges {
            if seen.insert(e.dependency) {
                queue.push_back(e.dependency);
            }
        }
    }
    Ok(out)
}

/// True if `derivation_id` already has at least one edge recorded in
/// `derivation_dependency`. Leaf derivations (drvs with no build-time
/// dependencies) therefore always look "unpopulated" and would be
/// re-traversed on every evaluation — that is cheap and correct.
async fn has_dependency_edges(state: &Arc<ServerState>, derivation_id: Uuid) -> Result<bool> {
    let n = EDerivationDependency::find()
        .filter(CDerivationDependency::Derivation.eq(derivation_id))
        .one(&state.db)
        .await
        .context("has_dependency_edges: query edges")?;
    Ok(n.is_some())
}

/// Queue entry: `(drv_path, optional parent derivation id)`. `parent_id`
/// is `None` for top-level entry points.
type QueueItem = (String, Option<Uuid>);

/// Builds up the accumulator starting from a set of root derivation paths.
///
/// For each drv path we find-or-create its `derivation` row; newly created
/// derivations are traversed via `query_pathinfo.references` and their
/// outputs / deps are recorded. For derivations that already have their
/// dep edges populated, we materialise build rows for the full in-DB
/// closure without touching the Nix store.
pub(super) async fn query_all_dependencies(
    state: Arc<ServerState>,
    acc: &mut EvaluationAccumulator,
    evaluation: &MEvaluation,
    organization_id: Uuid,
    roots: Vec<String>,
) -> Result<()> {
    // Map drv_path -> derivation_id to deduplicate within this evaluation.
    let mut derivation_by_path: HashMap<String, Uuid> = HashMap::new();
    // derivation_id -> whether we already created a build row this eval.
    let mut build_created: HashMap<Uuid, Uuid> = HashMap::new();

    let mut queue: VecDeque<QueueItem> = roots.into_iter().map(|p| (p, None)).collect();

    while let Some((drv_path, parent_derivation_id)) = queue.pop_front() {
        // Resolve or create the derivation row.
        let (derivation_id, was_new) = if let Some(id) = derivation_by_path.get(&drv_path) {
            (*id, false)
        } else if let Some(existing) = find_derivation(&state, organization_id, &drv_path).await? {
            derivation_by_path.insert(drv_path.clone(), existing.id);
            (existing.id, false)
        } else {
            // Need features + outputs from Nix to create a new derivation.
            let (arch, features) = state
                .derivation_resolver
                .get_features(drv_path.clone())
                .await
                .with_context(|| format!("get_features for {}", drv_path))?;

            let id = Uuid::new_v4();
            let now = Utc::now().naive_utc();
            let derivation = MDerivation {
                id,
                organization: organization_id,
                derivation_path: drv_path.clone(),
                architecture: arch,
                created_at: now,
            };
            acc.new_derivations.push(derivation);
            derivation_by_path.insert(drv_path.clone(), id);
            acc.pending_features.push((id, features));

            // Upsert outputs discovered from the store (multi-output
            // first-class). If the drv is not yet realised, this returns
            // []; outputs are then backfilled on first successful build.
            if let Ok(outputs) = state.nix_store.get_build_outputs(drv_path.clone()).await {
                for o in outputs {
                    let has_artefacts =
                        tokio::fs::metadata(format!("{}/nix-support/hydra-build-products", o.path))
                            .await
                            .is_ok();
                    acc.new_derivation_outputs.push(ADerivationOutput {
                        id: Set(Uuid::new_v4()),
                        derivation: Set(id),
                        name: Set(o.name),
                        output: Set(o.path),
                        hash: Set(o.hash),
                        package: Set(o.package),
                        ca: Set(o.ca),
                        file_hash: Set(None),
                        file_size: Set(None),
                        nar_size: Set(None),
                        is_cached: Set(false),
                        has_artefacts: Set(has_artefacts),
                        created_at: Set(now),
                    });
                }
            }
            (id, true)
        };

        // Record parent -> this dep edge (derivation-level, once per drv).
        if let Some(parent) = parent_derivation_id {
            // Dedup: the edge uniqueness index catches duplicates at DB
            // level, but we skip re-pushing if we've already pushed this
            // edge in this evaluation. A simple linear scan is fine for
            // the eval's working set.
            if !acc
                .new_derivation_dependencies
                .iter()
                .any(|d| d.derivation == parent && d.dependency == derivation_id)
            {
                acc.new_derivation_dependencies.push(MDerivationDependency {
                    id: Uuid::new_v4(),
                    derivation: parent,
                    dependency: derivation_id,
                });
            }
        }

        // Materialise a `build` row for this evaluation, unless we already
        // created one earlier in this traversal.
        let in_store = state
            .nix_store
            .query_missing_paths(vec![drv_path.clone()])
            .await
            .map(|missing| missing.is_empty())
            .unwrap_or(false);

        let build_id = *build_created.entry(derivation_id).or_insert_with(|| {
            let build = make_build_row(evaluation.id, derivation_id, in_store);
            let id = build.id;
            acc.builds.push(build);
            id
        });
        trace!(drv = %drv_path, %build_id, in_store, was_new, "Registered derivation");

        // Decide whether to traverse this derivation's references.
        if was_new {
            // Freshly created — we must walk its references from the Nix
            // store to populate derivation_dependency correctly.
            let Some(path_info) = state
                .nix_store
                .query_pathinfo(drv_path.clone())
                .await
                .context("query_pathinfo")?
            else {
                continue;
            };
            for r in path_info.references {
                let ref_path = strip_nix_store_prefix(&r);
                if ref_path == drv_path {
                    continue;
                }
                queue.push_back((ref_path, Some(derivation_id)));
            }
        } else if has_dependency_edges(&state, derivation_id).await? {
            // Existing derivation with recorded deps — walk the in-DB
            // closure and materialise build rows for every member.
            let closure = load_closure(&state, derivation_id).await?;
            for d in closure {
                if build_created.contains_key(&d.id) {
                    continue;
                }
                let dep_in_store = state
                    .nix_store
                    .query_missing_paths(vec![d.derivation_path.clone()])
                    .await
                    .map(|m| m.is_empty())
                    .unwrap_or(false);
                let build = make_build_row(evaluation.id, d.id, dep_in_store);
                build_created.insert(d.id, build.id);
                acc.builds.push(build);
                derivation_by_path.insert(d.derivation_path.clone(), d.id);
            }
        } else {
            // Existing derivation but no recorded deps — treat as leaf.
            // If it actually has build-time deps they will be discovered
            // the next time this drv is evaluated from scratch.
            debug!(drv = %drv_path, "Existing derivation with no recorded deps; not re-traversing");
        }
    }

    Ok(())
}
