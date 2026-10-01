/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The single definition of the recursive graph walks. Every traversal of
//! `derivation_dependency` (failure cascades, eval-closure sweeps, GC
//! reachability), build-time and runtime alike, is
//! generated here so the walkers can never disagree on what "reachable" means,
//! and so the join shape below has exactly one place to live.
//!
//! Postgres estimates a recursive CTE's working table at ten times the seed,
//! which for these walks overshoots by two orders of magnitude (348,870
//! estimated against 2,439 actual on a 44k-node eval closure). At that
//! cardinality a merge join against the whole edge index costs out cheaper than
//! a nested loop, so the planner rescans all four million edges once per
//! iteration. Every recursive term here is therefore written as a `LATERAL`
//! subquery with an `OFFSET 0` optimisation fence: the fence stops the planner
//! pulling the subquery back up, which leaves a nested loop with a per-row index
//! lookup as the only legal plan. Measured on production: eval closure 5,278 ms
//! to 955 ms, GC keep-set 40,069 ms to 9,746 ms, the runtime-reference walk
//! from over 180,000 ms to 18,425 ms.
//!
//! The set operator stays `UNION`. It is what deduplicates the frontier on each
//! iteration, and these graphs are diamond-heavy enough that the wanted-by walk
//! already emits 940k rows for 68k distinct nodes; `UNION ALL` would drop the
//! deduplication and make the walk exponential in depth.

use super::predicates::{builder_predicate, open_predicate};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait};

/// Raises `work_mem` for one walk. The `UNION` in every recursive term above
/// deduplicates the frontier through an in-memory hash, and the cluster default
/// of 4 MB is far under what these graphs need: the wanted-by walk alone emits
/// 940k rows for 68k distinct nodes, so the hash spills to a temporary file and
/// is re-spilled on every iteration.
///
/// `SET LOCAL` is the only safe form here. A bare `SET` on a pooled connection
/// outlives the statement that issued it and would raise the ceiling for every
/// later query that borrows the same connection; `SET LOCAL` reverts with the
/// transaction, and is silently ignored (with a server warning) outside one,
/// which is why [`begin_walk`] opens the transaction rather than trusting the
/// caller to be inside one.
///
/// The value has to stay ABOVE the cluster's own `work_mem` or this is an
/// expensive no-op: a raise to the number the floor already sits at buys the
/// walk nothing. Raising that floor instead is the wrong trade, since it
/// multiplies by every sort node of every concurrent query, which is why the
/// module leaves it to `services.gradient.postgres.workMem` rather than
/// guessing it.
pub const WALK_WORK_MEM: &str = "SET LOCAL work_mem = '64MB'";

/// Opens a transaction sized for one graph walk. The caller executes its statement
/// on the returned handle and commits; a dropped handle rolls back, which for a
/// read-only walk is equivalent. Handed a connection that already stands for an
/// open transaction (the graph writer's `WorkerDb`) this is a savepoint, so the
/// raise lasts to the end of that outer transaction rather than to the release.
pub async fn begin_walk<C>(db: &C) -> Result<DatabaseTransaction, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let txn = db.begin().await?;
    txn.execute_unprepared(WALK_WORK_MEM).await?;
    Ok(txn)
}

pub enum ClosureDirection {
    /// Walk from the roots toward the inputs they need (the build-time closure).
    Dependencies,
    /// Walk from the roots toward the shared builds that need them (wanted by).
    WantedBy,
}

/// A `WITH RECURSIVE {name}(derivation) AS (...)` prelude closing `seed_select`
/// over `derivation_dependency` in `direction`. The seed may contain UNION arms;
/// every arm must select exactly one derivation-id column.
pub fn dependency_closure_cte(
    name: &str,
    seed_select: &str,
    direction: ClosureDirection,
) -> String {
    format!(
        "WITH RECURSIVE {}",
        dependency_closure_cte_body(name, seed_select, direction)
    )
}

/// The bare `{name}(derivation) AS (...)` CTE body, without the `WITH RECURSIVE`
/// prefix, so a statement can bind several closures under one `WITH RECURSIVE`
/// (e.g. an eval closure plus the deterministic-blocked set it constrains).
pub fn dependency_closure_cte_body(
    name: &str,
    seed_select: &str,
    direction: ClosureDirection,
) -> String {
    bounded_dependency_closure_cte_body(name, seed_select, direction, "", None)
}

/// A closure walk with an extra predicate `bound` over the edge alias `e`,
/// applied inside the lateral probe so it prunes at the index lookup rather than
/// after the join; an empty `bound` is the unrestricted walk. `within` names a CTE
/// of derivations the walk stays inside (an eval closure), tested by
/// [`within_step`] outside the fenced probe.
pub fn bounded_dependency_closure_cte_body(
    name: &str,
    seed_select: &str,
    direction: ClosureDirection,
    bound: &str,
    within: Option<&str>,
) -> String {
    let (probe, project) = match direction {
        ClosureDirection::Dependencies => ("e.derivation", "e.dependency"),
        ClosureDirection::WantedBy => ("e.dependency", "e.derivation"),
    };
    let restrict = if bound.is_empty() {
        String::new()
    } else {
        format!(" AND {bound}")
    };
    let step = lateral_step(
        name,
        "s.next",
        &format!(
            "SELECT {project} AS next FROM derivation_dependency e \
             WHERE {probe} = c.derivation{restrict}"
        ),
    );
    let step = within_step(step, "t.next", within);
    format!("{name}(derivation) AS ({seed_select} UNION {step})")
}

/// Confine a recursive step to the CTE `within` by semi-joining each level's
/// frontier against it once. Inside the probe the membership is a subplan per
/// working-table row, and the planner sizes a recursive CTE far past its real
/// size, so it neither hashes it nor keeps it: every row rescans the whole set.
fn within_step(step: String, project: &str, within: Option<&str>) -> String {
    match within {
        None => step,
        Some(set) => format!(
            "SELECT {project} FROM ({step} OFFSET 0) t \
             WHERE EXISTS (SELECT 1 FROM {set} x WHERE x.derivation = t.next)"
        ),
    }
}

/// One fenced recursive term: join the working table `{name}` (aliased `c`) to
/// `probe_select` through a `LATERAL` subquery that `OFFSET 0` keeps the planner
/// from pulling up. See the module docs for why the fence is load-bearing rather
/// than decorative. `probe_select` projects a column aliased `next`, plus whatever
/// else `project` carries out of `s`, and correlates to the working-table row
/// through `c`.
fn lateral_step(name: &str, project: &str, probe_select: &str) -> String {
    format!("SELECT {project} FROM {name} c, LATERAL ({probe_select} OFFSET 0) s")
}

/// A `WITH RECURSIVE {name}(derivation) AS (...)` prelude closing `seed_select`
/// over the RUNTIME dependencies of the derivation graph: what a client must fetch
/// alongside an output, as opposed to the build-time closure the same relation
/// carries under `kind IN (0, 2)`.
pub fn runtime_closure_cte(name: &str, seed_select: &str) -> String {
    format!(
        "WITH RECURSIVE {}",
        runtime_closure_cte_body(name, seed_select)
    )
}

/// The bare `{name}(derivation) AS (...)` runtime-closure body, for statements
/// that bind it alongside another CTE.
pub fn runtime_closure_cte_body(name: &str, seed_select: &str) -> String {
    bounded_dependency_closure_cte_body(
        name,
        seed_select,
        ClosureDirection::Dependencies,
        "e.kind IN (1, 2)",
        None,
    )
}

/// Closure of the derivations an evaluation directly references (its
/// `build_job` rows), walking toward dependencies. Binds the evaluation id as
/// `$1`. Shared by every per-eval sweep so they all see the same closure.
pub fn eval_closure_cte() -> String {
    format!("WITH RECURSIVE {}", eval_closure_cte_body())
}

/// The eval-closure CTE body (no `WITH RECURSIVE` prefix), for statements that
/// bind it alongside a second closure under one `WITH RECURSIVE`.
pub fn eval_closure_cte_body() -> String {
    dependency_closure_cte_body(
        "closure",
        "SELECT bj.derivation FROM build_job bj WHERE bj.evaluation = $1",
        ClosureDirection::Dependencies,
    )
}

/// Dependency closure, over build and runtime dependencies alike, of the live GC roots
/// (`entry_point` and `build_job` derivations). A derivation in this set is still needed to build or serve a
/// retained closure and must never be reclaimed, even with no `build_job` of
/// its own: `build_job` rows are pruned with old evals while dependency edges
/// and shared builds persist.
pub fn reachable_derivations_cte() -> String {
    format!("WITH RECURSIVE {}", reachable_derivations_cte_body())
}

/// The reachable-roots CTE body (no `WITH RECURSIVE` prefix), for statements
/// that bind it alongside a second closure under one `WITH RECURSIVE`.
pub fn reachable_derivations_cte_body() -> String {
    dependency_closure_cte_body(
        "reachable",
        "SELECT derivation FROM entry_point UNION SELECT derivation FROM build_job",
        ClosureDirection::Dependencies,
    )
}

/// Every cached path a retained evaluation can reach, as [`kept_hashes_cte_body`]
/// names it over the reachable derivations and their runtime closure. This is the
/// cache's keep-set; everything outside it is the eviction pass's to reclaim once
/// past the fetch TTL.
pub fn live_cached_paths_cte() -> String {
    format!(
        "WITH RECURSIVE {reachable}, {runtime}, {kept}",
        reachable = reachable_derivations_cte_body(),
        runtime = runtime_closure_cte_body("runtime", "SELECT derivation FROM reachable"),
        kept = kept_hashes_cte_body("reachable", "runtime"),
    )
}

/// The `live(hash)` body over a reachable set and its runtime closure: the outputs
/// of everything the closure reaches, plus the `.drv` NAR and the `inputSrcs` of
/// every reachable derivation. The sources hang off the `.drv` and have no
/// producer of their own, so nothing else in the walk names them.
pub fn kept_hashes_cte_body(reachable: &str, runtime: &str) -> String {
    format!(
        "live(hash) AS (\
         SELECT o.hash FROM derivation_output o JOIN {runtime} t ON t.derivation = o.derivation \
         UNION \
         SELECT d.hash FROM derivation d JOIN {reachable} r ON r.derivation = d.id \
         UNION \
         SELECT s.hash FROM derivation_input_source s \
         JOIN {reachable} r ON r.derivation = s.derivation)"
    )
}

/// `WITH RECURSIVE {name}(evaluation, derivation, builder) AS (...)`, see
/// [`open_closure_cte_body`].
pub fn open_closure_cte(name: &str, seed_select: &str) -> String {
    format!(
        "WITH RECURSIVE {}",
        open_closure_cte_body(name, seed_select, None)
    )
}

/// The one walk that the need flag and naming are both projections of: from the
/// `(evaluation, derivation, builder)` rows of `seed_select`, every open shared build the
/// seeds still want. A member steps over its runtime dependencies, because whatever wants
/// a shared build wants what its outputs reference; a builder steps over every edge,
/// because it will be built and needs its inputs; and only an open shared build is
/// reached, so a fetchable input and a failed one both end the walk. A passthrough is
/// reached and never stepped through on a build edge, which is what keeps a passed-through
/// subtree from being built. There is no name guard: a name is what adoption writes
/// for what this reaches, and the need flag is what the recount writes for it. `within`
/// names a CTE of derivations the walk stays inside, tested outside the fenced
/// probe so each level is semi-joined against it once.
pub fn open_closure_cte_body(name: &str, seed_select: &str, within: Option<&str>) -> String {
    let step = lateral_step(
        name,
        "c.evaluation, s.next, s.builder",
        &format!(
            "SELECT e.dependency AS next, ({builder}) AS builder \
             FROM derivation_dependency e \
             JOIN derivation_build dep ON dep.derivation = e.dependency \
             JOIN derivation w ON w.id = dep.derivation \
             WHERE e.derivation = c.derivation AND (c.builder OR e.kind IN (1, 2)) \
               AND {open}",
            builder = builder_predicate("dep", "w"),
            open = open_predicate("dep"),
        ),
    );
    let step = within_step(step, "t.evaluation, t.next, t.builder", within);
    format!("{name}(evaluation, derivation, builder) AS ({seed_select} UNION {step})")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The live set walks the runtime dependencies out of every reachable derivation and
    /// keeps the outputs of everything they reach, plus each reachable
    /// derivation's own `.drv` and its `inputSrcs`, which hang off the `.drv` and
    /// have no producer of their own.
    #[test]
    fn live_cached_paths_close_reachable_outputs_and_drvs_over_references() {
        let cte = norm(&live_cached_paths_cte());

        assert!(
            cte.starts_with("WITH RECURSIVE reachable(derivation) AS ("),
            "{cte}"
        );
        assert!(
            cte.contains(
                "runtime(derivation) AS (SELECT derivation FROM reachable UNION SELECT s.next"
            ),
            "{cte}"
        );
        assert!(
            cte.contains(concat!(
                "live(hash) AS (SELECT o.hash FROM derivation_output o ",
                "JOIN runtime t ON t.derivation = o.derivation UNION ",
                "SELECT d.hash FROM derivation d JOIN reachable r ON r.derivation = d.id UNION ",
                "SELECT s.hash FROM derivation_input_source s ",
                "JOIN reachable r ON r.derivation = s.derivation)",
            )),
            "{cte}"
        );
    }

    /// The raise has to be `SET LOCAL` and it has to happen inside the walk's own
    /// transaction. A bare `SET` on a pooled connection outlives the walk and
    /// raises the ceiling for every unrelated statement that later borrows the
    /// same connection; `SET LOCAL` outside a transaction block is a no-op the
    /// server only warns about, so the walk cannot rely on the caller for one.
    #[tokio::test]
    async fn the_walk_raises_work_mem_with_set_local_inside_its_own_transaction() {
        assert!(WALK_WORK_MEM.starts_with("SET LOCAL "), "{WALK_WORK_MEM}");

        let db = sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres)
            .append_exec_results([sea_orm::MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        begin_walk(&db)
            .await
            .expect("the walk opens")
            .commit()
            .await
            .expect("the walk closes");

        let log = crate::pool::statements(db.into_transaction_log());
        assert_eq!(
            log.len(),
            1,
            "one statement, bracketed by the walk: {log:?}"
        );
        assert!(log[0].contains(WALK_WORK_MEM), "{log:?}");
    }

    /// The wanted-by direction must walk upward (a dependency edge leads to the
    /// shared builds that consume it) so failure cascades reach every consumer.
    #[test]
    fn wanted_by_walk_upward() {
        let cte = norm(&dependency_closure_cte(
            "wanted_by",
            "SELECT $1::uuid",
            ClosureDirection::WantedBy,
        ));
        assert!(
            cte.starts_with("WITH RECURSIVE wanted_by(derivation) AS"),
            "{cte}"
        );
        assert!(
            cte.contains(
                "SELECT e.derivation AS next FROM derivation_dependency e WHERE e.dependency = c.derivation"
            ),
            "must walk parents upward via the dependency edge: {cte}"
        );
    }

    /// Dependencies direction must walk downward (toward inputs) so keep-sets
    /// and per-eval sweeps cover the full build-time closure.
    #[test]
    fn dependencies_walk_downward() {
        let cte = norm(&eval_closure_cte());
        assert!(
            cte.starts_with("WITH RECURSIVE closure(derivation) AS"),
            "{cte}"
        );
        assert!(
            cte.contains("SELECT bj.derivation FROM build_job bj WHERE bj.evaluation = $1"),
            "{cte}"
        );
        assert!(
            cte.contains(
                "SELECT e.dependency AS next FROM derivation_dependency e WHERE e.derivation = c.derivation"
            ),
            "must recurse toward dependencies: {cte}"
        );
    }

    /// The orphan-GC keep-set must be the build-dependency closure of the live
    /// roots (entry_points + build_jobs), not just the roots themselves: a dep
    /// reached only through `derivation_dependency` (its own `build_job` pruned
    /// with an old eval) must survive.
    #[test]
    fn reachable_cte_closes_over_roots_and_dependency_edges() {
        let cte = norm(&reachable_derivations_cte());
        assert!(
            cte.contains("FROM entry_point"),
            "entry points are roots: {cte}"
        );
        assert!(
            cte.contains("FROM build_job"),
            "build_job derivations are roots: {cte}"
        );
        assert!(
            cte.contains("SELECT e.dependency AS next"),
            "recursion walks toward dependencies (the inputs a root needs): {cte}"
        );
    }

    /// The fence is the whole performance fix: without `LATERAL (... OFFSET 0)`
    /// the planner takes the recursive CTE's 10x working-table estimate at face
    /// value and merge-joins the entire edge table once per iteration. Assert it
    /// on every generated walk so a later tidy-up cannot quietly drop it.
    #[test]
    fn every_recursive_term_is_fenced_into_a_nested_loop() {
        for cte in [
            norm(&eval_closure_cte()),
            norm(&reachable_derivations_cte()),
            norm(&dependency_closure_cte(
                "wanted_by",
                "SELECT $1::uuid",
                ClosureDirection::WantedBy,
            )),
            norm(&runtime_closure_cte("refs", "SELECT $1::uuid")),
            norm(&open_closure_cte(
                "pending",
                "SELECT $1::uuid, $2::uuid, true",
            )),
        ] {
            assert!(
                cte.contains("LATERAL ("),
                "recursive term must be lateral: {cte}"
            );
            assert!(
                cte.contains("OFFSET 0) s"),
                "lateral probe must be fenced: {cte}"
            );
            assert!(
                !cte.contains("UNION ALL"),
                "UNION dedupes the frontier; UNION ALL is exponential on a diamond graph: {cte}"
            );
        }
    }

    /// A bounded walk must apply its restriction inside the lateral probe and
    /// ahead of the fence, so the frontier is pruned at the index lookup rather
    /// than after the join, and the `OFFSET 0` still terminates the subquery.
    #[test]
    fn a_bounded_walk_restricts_inside_the_probe() {
        let cte = norm(&bounded_dependency_closure_cte_body(
            "wanted_by",
            "SELECT $1::uuid",
            ClosureDirection::WantedBy,
            "e.kind IN (1, 2)",
            None,
        ));

        let probe = "WHERE e.dependency = c.derivation AND e.kind IN (1, 2) OFFSET 0) s";

        assert!(
            cte.contains(probe),
            "the bound belongs inside the fenced probe: {cte}"
        );
    }

    /// A walk inside an eval closure semi-joins each level against it outside the
    /// fence. As a subplan inside the probe, the planner (sizing the recursive CTE
    /// at millions of rows) rescanned the whole closure per working-table row: an
    /// unstick of a 10k-name nixos eval ran past 10 minutes, and 7 s this way.
    #[test]
    fn a_walk_within_a_closure_semi_joins_it_once_per_level() {
        let cte = norm(&bounded_dependency_closure_cte_body(
            "wanted_by",
            "SELECT $1::uuid",
            ClosureDirection::WantedBy,
            "e.kind IN (1, 2)",
            Some("closure"),
        ));

        assert!(
            cte.contains(
                "AND e.kind IN (1, 2) OFFSET 0) s OFFSET 0) t \
                 WHERE EXISTS (SELECT 1 FROM closure x WHERE x.derivation = t.next))"
            ),
            "the closure test sits outside the fenced probe: {cte}"
        );
        assert!(
            !cte.contains("IN (SELECT derivation FROM closure)"),
            "no closure membership is a subplan: {cte}"
        );
    }

    /// The runtime closure walks what a client must fetch alongside an output,
    /// not the build inputs, so it is the same relation restricted to the runtime
    /// edge kinds.
    #[test]
    fn the_runtime_closure_walks_runtime_dependencies_only() {
        let cte = norm(&runtime_closure_cte("eval_paths", "SELECT $1::uuid"));

        assert!(
            cte.starts_with("WITH RECURSIVE eval_paths(derivation) AS"),
            "{cte}"
        );
        assert!(
            cte.contains(
                "SELECT e.dependency AS next FROM derivation_dependency e \
                 WHERE e.derivation = c.derivation AND e.kind IN (1, 2) OFFSET 0) s"
            ),
            "{cte}"
        );
    }

    /// The keep-set is a derivation-level walk now: the outputs of everything the
    /// runtime closure reaches, plus the `.drv` and the `inputSrcs` of every
    /// reachable derivation, which hang off the `.drv` and have no producer.
    #[test]
    fn the_live_set_walks_runtime_dependencies_from_live_derivations_and_keeps_their_sources() {
        let cte = norm(&live_cached_paths_cte());
        assert!(cte.contains("e.kind IN (1, 2)"), "{cte}");
        assert!(
            cte.contains("SELECT s.hash FROM derivation_input_source s JOIN"),
            "{cte}"
        );
        assert!(!cte.contains("cached_path_reference"), "{cte}");
    }

    /// The wedge this walk replaces two walks for: 47 builders waited on 22
    /// `Completed` shared builds with a missing dependency in their closure, and it was never
    /// reached because one walk stepped only out of a NAMED member and the other
    /// reached only a builder STATUS. Neither guard may come back.
    #[test]
    fn an_incomplete_terminal_shared_build_is_reached_and_stepped_through() {
        let cte = norm(&open_closure_cte(
            "pending",
            "SELECT $1::uuid, $2::uuid, true",
        ));
        assert!(
            !cte.contains("build_job"),
            "a name is what adoption writes for what the walk reaches: {cte}"
        );
        assert!(
            cte.contains(&format!("AND {} OFFSET 0) s", norm(&open_predicate("dep")))),
            "reach is open, never a status list: {cte}"
        );
        assert!(
            !cte.contains("AND dep.status IN (0, 1, 2, 8, 10)"),
            "the old reach set must not come back: {cte}"
        );
    }

    /// A walk kept inside a set tests membership outside the fenced probe, so the
    /// planner semi-joins the whole level against the set once instead of
    /// re-reading the set for every working-table row.
    #[test]
    fn a_walk_within_a_set_semi_joins_it_once_per_level() {
        let cte = norm(&open_closure_cte_body(
            "wanted",
            "SELECT NULL::uuid, r.derivation, r.builder FROM region r",
            Some("region"),
        ));
        assert!(
            cte.contains(&format!(
                "AND {} OFFSET 0) s OFFSET 0) t \
                 WHERE EXISTS (SELECT 1 FROM region x WHERE x.derivation = t.next))",
                norm(&open_predicate("dep"))
            )),
            "{cte}"
        );
        assert!(
            norm(&open_closure_cte_body("x", "SELECT 1", None)).ends_with(&format!(
                "AND {} OFFSET 0) s)",
                norm(&open_predicate("dep"))
            )),
            "no set is the unrestricted walk"
        );
    }
}
