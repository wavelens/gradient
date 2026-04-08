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
//!
//! The accumulator is `Arc`-shared so multiple BFS walkers can run in
//! parallel. Concurrent resolution of the same drv path is serialised
//! through a per-path `tokio::sync::OnceCell`, so the DB's
//! `UNIQUE (organization, derivation_path)` index is never hit twice.

use anyhow::{Context, Result};
use chrono::Utc;
use entity::build::BuildStatus;
use gradient_core::executer::*;
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::OnceCell;
use tracing::{debug, trace};
use uuid::Uuid;

/// Whether the derivation was freshly inserted into the accumulator
/// during this evaluation or was already present in the DB from a
/// previous one. Determines the BFS traversal strategy for the
/// initialising walker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CreationState {
    /// `find_derivation` returned None — we pushed a new `MDerivation`
    /// into `new_derivations`. Initialiser must walk pathinfo to
    /// populate the dep edges for this derivation's subtree.
    FreshInEval,
    /// `find_derivation` returned Some — row already persisted.
    /// Initialiser should walk the in-DB closure to materialise build
    /// rows for every reachable member.
    ExistingInDB,
}

/// Accumulates newly-created rows across derivation processing. Every
/// field is guarded by a short-lived `std::sync::Mutex` — none are held
/// across `.await` points. Everything is bulk-inserted by the caller
/// once evaluation completes so scheduling cannot race against partial
/// state.
pub(super) struct SharedAccumulator {
    builds: StdMutex<Vec<MBuild>>,
    new_derivations: StdMutex<Vec<MDerivation>>,
    new_derivation_outputs: StdMutex<Vec<ADerivationOutput>>,
    new_derivation_dependencies: StdMutex<Vec<MDerivationDependency>>,
    entry_point_build_ids: StdMutex<Vec<(Uuid, String)>>,
    pending_features: StdMutex<Vec<(Uuid, Vec<String>)>>,

    /// drv_path -> resolved derivation id. Single-flight via per-path
    /// `OnceCell`: concurrent tasks hitting the same path all await
    /// the same cell; only one runs `find_or_create_inner`.
    derivation_cells:
        StdMutex<HashMap<String, Arc<OnceCell<(Uuid, CreationState)>>>>,

    /// derivation_id -> build_id registered for this evaluation.
    build_by_derivation: StdMutex<HashMap<Uuid, Uuid>>,
    /// Dedup for `(parent, child)` edges pushed this eval — the DB's
    /// UNIQUE index would also reject dupes, but we pre-filter to
    /// avoid the round-trip.
    edge_set: StdMutex<HashSet<(Uuid, Uuid)>>,
    /// Mirror map: drv_path -> derivation_id. Used by the root
    /// entry-point lookup in `evaluate()` and by the load_closure
    /// fast-path.
    path_to_derivation: StdMutex<HashMap<String, Uuid>>,
}

impl SharedAccumulator {
    pub(super) fn new() -> Self {
        Self {
            builds: StdMutex::new(Vec::new()),
            new_derivations: StdMutex::new(Vec::new()),
            new_derivation_outputs: StdMutex::new(Vec::new()),
            new_derivation_dependencies: StdMutex::new(Vec::new()),
            entry_point_build_ids: StdMutex::new(Vec::new()),
            pending_features: StdMutex::new(Vec::new()),
            derivation_cells: StdMutex::new(HashMap::new()),
            build_by_derivation: StdMutex::new(HashMap::new()),
            edge_set: StdMutex::new(HashSet::new()),
            path_to_derivation: StdMutex::new(HashMap::new()),
        }
    }

    /// Pushes a new `MDerivation` + its outputs + pending features.
    /// Synchronous (no await); the caller already did the async Nix
    /// queries before calling this.
    fn push_new_derivation(
        &self,
        derivation: MDerivation,
        outputs: Vec<ADerivationOutput>,
        features: Vec<String>,
    ) {
        let derivation_id = derivation.id;
        self.path_to_derivation
            .lock()
            .unwrap()
            .insert(derivation.derivation_path.clone(), derivation_id);
        self.new_derivations.lock().unwrap().push(derivation);
        {
            let mut out = self.new_derivation_outputs.lock().unwrap();
            out.extend(outputs);
        }
        self.pending_features
            .lock()
            .unwrap()
            .push((derivation_id, features));
    }

    /// Records path -> id for an already-persisted derivation.
    fn remember_existing(&self, drv_path: &str, id: Uuid) {
        self.path_to_derivation
            .lock()
            .unwrap()
            .insert(drv_path.to_string(), id);
    }

    /// Registers a `build` row for the given derivation id, or returns
    /// the existing one if we already made one this eval. The
    /// `make_build` closure is called at most once.
    fn register_build(&self, derivation_id: Uuid, make_build: impl FnOnce() -> MBuild) -> Uuid {
        let mut map = self.build_by_derivation.lock().unwrap();
        if let Some(id) = map.get(&derivation_id) {
            return *id;
        }
        let build = make_build();
        let build_id = build.id;
        map.insert(derivation_id, build_id);
        drop(map);
        self.builds.lock().unwrap().push(build);
        build_id
    }

    /// Adds a dep edge if it hasn't been pushed yet this eval.
    fn push_edge(&self, parent: Uuid, child: Uuid) {
        let mut set = self.edge_set.lock().unwrap();
        if set.insert((parent, child)) {
            drop(set);
            self.new_derivation_dependencies
                .lock()
                .unwrap()
                .push(MDerivationDependency {
                    id: Uuid::new_v4(),
                    derivation: parent,
                    dependency: child,
                });
        }
    }

    pub(super) fn push_entry_point(&self, build_id: Uuid, wildcard: String) {
        self.entry_point_build_ids
            .lock()
            .unwrap()
            .push((build_id, wildcard));
    }

    pub(super) fn lookup_build_for_path(&self, drv_path: &str) -> Option<Uuid> {
        let id = self.path_to_derivation.lock().unwrap().get(drv_path).copied()?;
        self.build_by_derivation.lock().unwrap().get(&id).copied()
    }

    /// Drains every accumulator buffer and returns the contents. Call
    /// once at the end of `evaluate()` — any further pushes would be
    /// lost.
    pub(super) fn into_parts(
        self: Arc<Self>,
    ) -> (
        Vec<MBuild>,
        Vec<MDerivation>,
        Vec<ADerivationOutput>,
        Vec<MDerivationDependency>,
        Vec<(Uuid, String)>,
        Vec<(Uuid, Vec<String>)>,
    ) {
        // We are the only remaining strong ref by the time the caller
        // drains. Even if not, cloning out of the mutexes is cheap
        // and safe.
        let builds = std::mem::take(&mut *self.builds.lock().unwrap());
        let new_derivations = std::mem::take(&mut *self.new_derivations.lock().unwrap());
        let new_derivation_outputs =
            std::mem::take(&mut *self.new_derivation_outputs.lock().unwrap());
        let new_derivation_dependencies =
            std::mem::take(&mut *self.new_derivation_dependencies.lock().unwrap());
        let entry_point_build_ids =
            std::mem::take(&mut *self.entry_point_build_ids.lock().unwrap());
        let pending_features = std::mem::take(&mut *self.pending_features.lock().unwrap());
        (
            builds,
            new_derivations,
            new_derivation_outputs,
            new_derivation_dependencies,
            entry_point_build_ids,
            pending_features,
        )
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

/// Single-flight resolution of a drv_path to its derivation id. The
/// first caller for a given path runs the find-or-create logic; all
/// others await the same `OnceCell` and receive the cached result.
///
/// Returns `(derivation_id, is_initializer, creation_state)`. Only the
/// initializer should walk the subtree (pathinfo for `FreshInEval`,
/// load_closure for `ExistingInDB`); concurrent callers for the same
/// drv get `is_initializer = false` and should only register the dep
/// edge + build row for this single node.
async fn resolve_derivation_id(
    state: &Arc<ServerState>,
    acc: &Arc<SharedAccumulator>,
    organization_id: Uuid,
    drv_path: &str,
) -> Result<(Uuid, bool, CreationState)> {
    // Get or insert the cell under the short map lock.
    let cell = {
        let mut map = acc.derivation_cells.lock().unwrap();
        map.entry(drv_path.to_string())
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone()
    };

    let is_init = Arc::new(AtomicBool::new(false));
    let is_init_clone = Arc::clone(&is_init);
    let state_clone = Arc::clone(state);
    let acc_clone = Arc::clone(acc);
    let drv_clone = drv_path.to_string();

    let (id, creation) = *cell
        .get_or_try_init(move || async move {
            is_init_clone.store(true, Ordering::SeqCst);
            find_or_create_inner(&state_clone, &acc_clone, organization_id, &drv_clone).await
        })
        .await?;

    Ok((id, is_init.load(Ordering::SeqCst), creation))
}

/// The init closure for `resolve_derivation_id`. Runs exactly once per
/// (drv_path, evaluation) thanks to the enclosing `OnceCell`.
async fn find_or_create_inner(
    state: &Arc<ServerState>,
    acc: &Arc<SharedAccumulator>,
    organization_id: Uuid,
    drv_path: &str,
) -> Result<(Uuid, CreationState)> {
    if let Some(existing) = find_derivation(state, organization_id, drv_path).await? {
        acc.remember_existing(drv_path, existing.id);
        return Ok((existing.id, CreationState::ExistingInDB));
    }

    // Need features + outputs from Nix to create a new derivation.
    let (arch, features) = state
        .derivation_resolver
        .get_features(drv_path.to_string())
        .await
        .with_context(|| format!("get_features for {}", drv_path))?;

    let id = Uuid::new_v4();
    let now = Utc::now().naive_utc();
    let derivation = MDerivation {
        id,
        organization: organization_id,
        derivation_path: drv_path.to_string(),
        architecture: arch,
        created_at: now,
    };

    // Upsert outputs discovered from the store (multi-output first-class).
    // If the drv is not yet realised, this returns []; outputs are then
    // backfilled on first successful build.
    let mut outputs_vec: Vec<ADerivationOutput> = Vec::new();
    if let Ok(outputs) = state.nix_store.get_build_outputs(drv_path.to_string()).await {
        for o in outputs {
            let has_artefacts =
                tokio::fs::metadata(format!("{}/nix-support/hydra-build-products", o.path))
                    .await
                    .is_ok();
            outputs_vec.push(ADerivationOutput {
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

    acc.push_new_derivation(derivation, outputs_vec, features);
    Ok((id, CreationState::FreshInEval))
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
    acc: Arc<SharedAccumulator>,
    evaluation: &MEvaluation,
    organization_id: Uuid,
    roots: Vec<String>,
) -> Result<()> {
    let mut queue: VecDeque<QueueItem> = roots.into_iter().map(|p| (p, None)).collect();

    while let Some((drv_path, parent_derivation_id)) = queue.pop_front() {
        let (derivation_id, is_initializer, creation) =
            resolve_derivation_id(&state, &acc, organization_id, &drv_path).await?;

        // Record parent -> this dep edge (derivation-level).
        if let Some(parent) = parent_derivation_id {
            acc.push_edge(parent, derivation_id);
        }

        // Decide in-store status and register the build row. Doing the
        // nix-store query even when another walker may have registered
        // the build id is fine — `register_build` is dedup'd and the
        // closure is only called if we are actually going to insert.
        let already_registered = acc
            .build_by_derivation
            .lock()
            .unwrap()
            .contains_key(&derivation_id);
        let build_id = if already_registered {
            acc.register_build(derivation_id, || unreachable!())
        } else {
            let in_store = state
                .nix_store
                .query_missing_paths(vec![drv_path.clone()])
                .await
                .map(|missing| missing.is_empty())
                .unwrap_or(false);
            acc.register_build(derivation_id, || {
                make_build_row(evaluation.id, derivation_id, in_store)
            })
        };
        trace!(
            drv = %drv_path,
            %build_id,
            is_initializer,
            ?creation,
            "Registered derivation"
        );

        // Only the initializing walker traverses downstream. Other
        // walkers hitting the same path just added the edge + build
        // for this single node and move on.
        if !is_initializer {
            continue;
        }

        match creation {
            CreationState::FreshInEval => {
                // Walk pathinfo from the Nix store to populate the new
                // derivation's dep edges.
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
            }
            CreationState::ExistingInDB => {
                if has_dependency_edges(&state, derivation_id).await? {
                    // Existing derivation with recorded deps — walk the
                    // in-DB closure and materialise build rows for every
                    // member.
                    let closure = load_closure(&state, derivation_id).await?;
                    for d in closure {
                        let already = acc
                            .build_by_derivation
                            .lock()
                            .unwrap()
                            .contains_key(&d.id);
                        if already {
                            acc.remember_existing(&d.derivation_path, d.id);
                            continue;
                        }
                        let dep_in_store = state
                            .nix_store
                            .query_missing_paths(vec![d.derivation_path.clone()])
                            .await
                            .map(|m| m.is_empty())
                            .unwrap_or(false);
                        acc.register_build(d.id, || {
                            make_build_row(evaluation.id, d.id, dep_in_store)
                        });
                        acc.remember_existing(&d.derivation_path, d.id);
                    }
                } else {
                    debug!(drv = %drv_path, "Existing derivation with no recorded deps; not re-traversing");
                }
            }
        }
    }

    Ok(())
}
