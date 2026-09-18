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
//! iteration, and these graphs are diamond-heavy enough that the dependents walk
//! already emits 940k rows for 68k distinct nodes; `UNION ALL` would drop the
//! deduplication and make the walk exponential in depth.

use gradient_entity::build::BuildStatus;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait};

/// Raises `work_mem` for one walk. The `UNION` in every recursive term above
/// deduplicates the frontier through an in-memory hash, and the cluster default
/// of 4 MB is far under what these graphs need: the dependents walk alone emits
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
/// module leaves it to `services.gradient.server.postgresWorkMem` rather than
/// guessing it.
pub const WALK_WORK_MEM: &str = "SET LOCAL work_mem = '64MB'";

/// Opens a transaction sized for one graph walk. The caller runs its statement
/// on the returned handle and commits; a dropped handle rolls back, which for a
/// read-only walk is equivalent. Handed a connection that already stands for an
/// open transaction (the graph actor's `WorkerDb`) this is a savepoint, so the
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
    /// Walk from the roots toward the anchors that need them (the dependents).
    Dependents,
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
    bounded_dependency_closure_cte_body(name, seed_select, direction, "")
}

/// A closure walk confined to a set another CTE in the same statement already
/// binds (an eval closure, a requeue candidate set). `bound` is an extra
/// predicate over the edge alias `e`, applied inside the lateral probe so it
/// prunes at the index lookup rather than after the join; an empty `bound` is
/// the unrestricted walk.
pub fn bounded_dependency_closure_cte_body(
    name: &str,
    seed_select: &str,
    direction: ClosureDirection,
    bound: &str,
) -> String {
    let (probe, project) = match direction {
        ClosureDirection::Dependencies => ("e.derivation", "e.dependency"),
        ClosureDirection::Dependents => ("e.dependency", "e.derivation"),
    };
    let restrict = if bound.is_empty() {
        String::new()
    } else {
        format!(" AND {bound}")
    };
    format!(
        "{name}(derivation) AS ({seed_select} UNION {})",
        lateral_step(
            name,
            "s.next",
            &format!(
                "SELECT {project} AS next FROM derivation_dependency e \
                 WHERE {probe} = c.derivation{restrict}"
            ),
        )
    )
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
/// over the RUNTIME edges of the derivation graph: what a client must fetch
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
    )
}

/// The build target `{alias}`'s own `.drv` NAR is in our cache. Its closure is
/// trusted: the evaluation pushed it before it reported the derivation, so this is
/// the "the worker can fetch and import the input-`.drv` closure" signal. The exact
/// negation of [`drv_nar_absent_predicate`], which is what condemns an evaluation.
pub fn drv_present_predicate(alias: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM derivation d JOIN cached_path cp ON cp.hash = d.hash \
         WHERE d.id = {alias}.derivation AND cp.file_hash IS NOT NULL)"
    )
}

/// The build target `{alias}`'s own `.drv` NAR is not in our cache at all: no
/// `cached_path` row, or a row with no backing NAR. This is the only `.drv`
/// state a fresh evaluation repairs - it re-materialises and re-uploads the
/// `.drv`. The exact negation of [`drv_present_predicate`]: a `.drv` that is
/// present but whose closure has a hole is deliberately NOT this, because
/// re-evaluating cannot fetch that hole, and conflating the two burned an
/// evaluation per stall and then failed it as unrecoverable with the `.drv`
/// cached the whole time.
pub fn drv_nar_absent_predicate(alias: &str) -> String {
    format!(
        r#"NOT EXISTS (
        SELECT 1 FROM derivation d
        JOIN cached_path cp ON cp.hash = d.hash
        WHERE d.id = {alias}.derivation AND cp.file_hash IS NOT NULL)"#
    )
}

/// The anchor `{alias}`'s derivation has its full record in. Promotion and
/// dispatch require it: an anchor whose edges are not all recorded would
/// otherwise be queued as dependency-free.
pub fn walked_predicate(alias: &str) -> String {
    format!("EXISTS (SELECT 1 FROM derivation w WHERE w.id = {alias}.derivation AND w.walked)")
}

/// The statuses of an anchor that will still be built, and so still needs its
/// inputs. Closed under both promotion moves (`Created` to `Queued` and back), so a
/// promotion can never change whether a dependent demands what it walks over.
pub const BUILDER_STATUSES: [BuildStatus; 4] = [
    BuildStatus::Created,
    BuildStatus::Queued,
    BuildStatus::Building,
    BuildStatus::FailedTransient,
];

/// Anchor `status`/`demanded` is work its evaluation is still waiting for: what
/// nothing demands is never promoted, so an evaluation that counts it as pending
/// waits forever (#666). Every terminal status is settled by definition, and an
/// anchor the dispatcher can still pick up - or already has - is work in flight
/// whatever demand says now: dispatch reads the status rather than the gate, and
/// a running build is deliberately left to finish.
pub fn blocks_evaluation(status: BuildStatus, demanded: bool) -> bool {
    if !BUILDER_STATUSES.contains(&status) {
        return false;
    }

    demanded || matches!(status, BuildStatus::Queued | BuildStatus::Building)
}

/// [`blocks_evaluation`] as a predicate on anchor `{alias}`, so the decision has
/// one definition and the reader that asks the database for it cannot drift from
/// the reader that evaluates it in Rust.
pub fn blocks_evaluation_predicate(alias: &str) -> String {
    format!(
        "({alias}.status IN ({pending}) AND ({alias}.demanded OR {alias}.status IN ({in_flight})))",
        pending = crate::status_sql::build_in(&BUILDER_STATUSES),
        in_flight = crate::status_sql::build_in(&[BuildStatus::Queued, BuildStatus::Building]),
    )
}

/// Anchor `{anchor}` (its `derivation` row aliased `{walked}`) is a builder:
/// recorded, not a relay, and in a status an evaluation will still have built.
/// The one definition of what demands its inputs and of what the adoption walk
/// steps through, so the two can never disagree.
pub fn builder_predicate(anchor: &str, walked: &str) -> String {
    format!(
        "{walked}.walked AND NOT {anchor}.substitutable AND {anchor}.status IN ({pending})",
        pending = crate::status_sql::build_in(&BUILDER_STATUSES),
    )
}

/// The anchor on derivation `{derivation}` still needs its inputs: nothing relays
/// it. A failure walk stops at one that does, because a substitutable anchor takes
/// finished bytes off an upstream - an input that can never build neither dooms it
/// nor reaches anything above it. Same rule as [`gates_predicate`]'s relay arm,
/// which lets a relay through with its `unready_deps` unread, and as the refusal in
/// `graph::policy` to mark a failing relay `Permanent`.
pub fn unrelayed_predicate(derivation: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM derivation_build rb \
         WHERE rb.derivation = {derivation} AND rb.substitutable)"
    )
}

/// Dependents can get the outputs of anchor `{alias}` from OUR cache: it reached
/// terminal success and every output is whole here. An upstream copy does not
/// count (#593): a dependent of an unrelayed substitutable anchor waits for the
/// relay, so a build pulls every input out of our own cache and the relay is on
/// the critical path exactly once, instead of every dependent re-fetching from
/// the upstream itself. This is the one readiness fact a dependent reads, and it
/// recurses over nothing - a dependency's own dependencies are already summarised
/// in its `unready_deps`.
///
/// The `EXISTS` over `derivation_output` is load-bearing and not a tautology. The
/// `NOT EXISTS` under it is vacuously true for an anchor with NO output rows, so
/// without the guard a terminal-success anchor whose outputs were never recorded
/// reads as fetchable, stops counting toward its dependents' `unready_deps`, and
/// those dependents are promoted and dispatched against an input nothing can
/// provide: the unbacked-output dead zone, measured on a live cluster.
pub fn fetchable_predicate(alias: &str) -> String {
    format!(
        "({alias}.status IN ({terminal_success}) AND {whole})",
        terminal_success = crate::status_sql::build_in(&BuildStatus::TERMINAL_SUCCESS),
        whole = anchor_whole_predicate(alias),
    )
}

/// Every output of `{alias}` has its NAR in our cache.
///
/// The `EXISTS` is load-bearing and not a tautology. The `NOT EXISTS` under it is
/// vacuously true for an anchor with NO output rows, so without the guard a
/// terminal-success anchor whose outputs were never recorded reads as present,
/// stops counting toward its dependents' `unready_deps`, and those dependents are
/// promoted and dispatched against an input nothing can provide: the
/// unbacked-output dead zone, measured on a live cluster.
pub fn present_predicate(alias: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = {alias}.derivation) \
         AND NOT EXISTS (SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash \
                         WHERE o.derivation = {alias}.derivation AND cp.file_hash IS NULL))"
    )
}

/// Present, and every runtime edge leads to a whole anchor: the whole runtime
/// closure of `{alias}`'s outputs is in our cache. The counter is moved by
/// [`crate::runtime_readiness`], never derived here, for the reason `unready_deps`
/// is: wholeness is transitive and a per-row predicate that looks one hop cannot
/// carry it.
pub fn anchor_whole_predicate(alias: &str) -> String {
    format!(
        "({alias}.missing_runtime_deps = 0 AND {present})",
        present = present_predicate(alias),
    )
}

/// The gates a `Created` anchor must pass to be queued, minus its own status term:
/// walked, named by some evaluation, still demanded, then one arm per kind of work.
/// A relay needs nothing more - it fetches finished bytes, so neither its inputs nor
/// its `.drv` matter. A build needs every input fetchable from our cache and its own
/// `.drv` importable.
///
/// Demand is a column, not a walk: [`crate::readiness::recompute_demand`] rewrites it
/// on the events that change it and the consistency sweep recomputes it absolutely.
/// Reading it here is what makes it transitive, which the one-hop `EXISTS` this
/// replaced could not be - a `Created` dependent counts as a builder, so the first
/// undemanded one re-demanded everything below it and a relayed anchor's whole input
/// closure was built (#666). It also takes a correlated three-table subquery off
/// every promote, un-promote and sweep row.
///
/// It must stay free of any reference to `{alias}`'s OWN `status`, which is why that
/// term lives in [`promotable_predicate`] instead. `m20260908_000002` and
/// `m20260909_000001` run a demote and a promote in sequence in one transaction and
/// they cannot interfere only because this never reads the column the demote writes;
/// the same holds for `readiness::repair_pending`.
pub fn gates_predicate(alias: &str) -> String {
    format!(
        r#"({walked}
    AND EXISTS (SELECT 1 FROM build_job bj WHERE bj.derivation = {alias}.derivation)
    AND {alias}.demanded
    AND ({alias}.substitutable
         OR ({alias}.unready_deps = 0 AND {drv_present})))"#,
        walked = walked_predicate(alias),
        drv_present = drv_present_predicate(alias),
    )
}

/// [`gates_predicate`] on a `Created` anchor: what promotion writes.
///
/// Dispatch does not re-derive this: it reads the status. So the invariant rests on
/// one rule, which every writer of `Queued` obeys in one of two ways: embed this
/// predicate in the write, or settle the rows just written with
/// [`crate::readiness::unpromote_ungated`] in the same call.
/// [`crate::readiness::repair_pending`] is the backstop for a counter that drifted
/// under a lost move.
pub fn promotable_predicate(alias: &str) -> String {
    format!(
        "({alias}.status = {created} AND {gates})",
        created = crate::status_sql::build(BuildStatus::Created),
        gates = gates_predicate(alias),
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

/// Build-dependency closure of the live GC roots (`entry_point` and `build_job`
/// derivations). A derivation in this set is still needed to build or serve a
/// retained closure and must never be reclaimed, even with no `build_job` of
/// its own: `build_job` rows are pruned with old evals while dependency edges
/// and anchors persist.
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

/// Every cached path a retained evaluation can reach: the outputs and `.drv`
/// NARs of the reachable derivations, closed over their references. Input
/// sources are references of the `.drv` NAR, so the walk from `derivation.hash`
/// covers them. This is the cache's keep-set; everything outside it is the
/// eviction pass's to reclaim once past the fetch TTL.
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

/// `WITH RECURSIVE pending(evaluation, derivation, builder) AS (...)`: from the
/// `(evaluation, derivation, builder)` rows of `seed_select`, every anchor in a
/// builder status that the evaluation reaches walking dependencies. Two arms over
/// the two edge kinds, mirroring [`demand_closure_cte`]: every member steps over
/// its runtime edges, because whatever wants an anchor wants what its outputs
/// reference, and only a builder steps over its build edges, since a relay fetches
/// finished bytes and waits on nothing below it. A terminal anchor stops the walk
/// either way, because what is below a finished build is served from its outputs
/// and what is below a failed one is the requeue's to thaw first. This is what
/// names a pruned subtree for the evaluations that build against it.
pub fn pending_closure_cte(name: &str, seed_select: &str) -> String {
    let arm = |restrict: &str| {
        format!(
            "SELECT e.dependency AS next, ({builder}) AS builder \
             FROM derivation_dependency e \
             JOIN derivation_build dep ON dep.derivation = e.dependency \
             JOIN derivation w ON w.id = dep.derivation \
             WHERE e.derivation = c.derivation AND {restrict} AND dep.status IN ({pending})",
            builder = builder_predicate("dep", "w"),
            pending = crate::status_sql::build_in(&BUILDER_STATUSES),
        )
    };

    format!(
        "WITH RECURSIVE {name}(evaluation, derivation, builder) AS ({seed_select} UNION {})",
        lateral_step(
            name,
            "c.evaluation, s.next, s.builder",
            &format!(
                "{runtime} UNION {build}",
                runtime = arm("e.kind IN (1, 2)"),
                build = arm("c.builder AND e.kind IN (0, 2)"),
            ),
        )
    )
}

/// `WITH RECURSIVE demanded(derivation) AS (...)`: from `seed_select`, every anchor
/// something still wants in our cache. Two arms over the two edge kinds. Anything
/// wanted wants what its outputs reference at run time, so a demanded anchor named
/// by a `build_job` steps over its runtime edges whatever it is. Only something
/// that will be built wants its inputs, so a demanded builder steps over its build
/// edges; a relay is reached and never stepped through that way. A terminal anchor
/// stops the walk because what is below a finished build is served from its
/// outputs. The one definition of demand; every recompute steps with it.
/// `region_select` bounds both arms, applied inside the probe so a region-scoped
/// recompute prunes at the index lookup instead of walking the live graph and
/// discarding it.
pub fn demand_closure_cte(seed_select: &str, region_select: &str) -> String {
    let bound = if region_select.is_empty() {
        String::new()
    } else {
        format!(" AND e.dependency IN ({region_select})")
    };
    let arm = |restrict: String| {
        format!(
            "SELECT e.dependency AS next FROM derivation_dependency e \
             JOIN derivation_build p ON p.derivation = c.derivation \
             {restrict} \
               AND EXISTS (SELECT 1 FROM build_job bj \
                           WHERE bj.derivation = p.derivation){bound}"
        )
    };

    format!(
        "WITH RECURSIVE demanded(derivation) AS ({seed_select} UNION {})",
        lateral_step(
            "demanded",
            "s.next",
            &format!(
                "{runtime} UNION {build}",
                runtime = arm("WHERE e.derivation = c.derivation AND e.kind IN (1, 2)".to_owned()),
                build = arm(format!(
                    "JOIN derivation w ON w.id = p.derivation \
                     WHERE e.derivation = c.derivation AND e.kind IN (0, 2) AND {builder}",
                    builder = builder_predicate("p", "w"),
                )),
            ),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The live set walks the runtime edges out of every reachable derivation and
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

    /// An anchor nothing demands is never promoted, so an evaluation that waits
    /// for it never finishes: it is settled work, not pending work. `Queued` and
    /// `Building` are the exception in the other direction - the dispatcher reads
    /// the status, so both can still produce a build after the demand is gone.
    #[test]
    fn only_demanded_or_in_flight_work_blocks_an_evaluation() {
        use BuildStatus::*;

        for status in BUILDER_STATUSES {
            assert!(
                blocks_evaluation(status, true),
                "{status:?} is pending work something wants"
            );
        }
        for status in [Queued, Building] {
            assert!(
                blocks_evaluation(status, false),
                "{status:?} is handed out on its status alone, demand or not"
            );
        }
        for status in [Created, FailedTransient] {
            assert!(
                !blocks_evaluation(status, false),
                "{status:?} with nothing demanding it is work that never happens"
            );
        }
        for status in [Completed, Substituted, FailedPermanent, DependencyFailed] {
            assert!(!blocks_evaluation(status, true), "{status:?} is settled");
        }
    }

    /// The database asks the same question the Rust does, so the two must name the
    /// same statuses. Eval-done reads the predicate and nothing else now: a drift
    /// between them would settle an evaluation whose builds are still running,
    /// with no test failing on either side alone.
    #[test]
    fn the_predicate_names_what_blocks_evaluation_names() {
        use BuildStatus::*;

        let sql = blocks_evaluation_predicate("db");
        assert_eq!(
            sql,
            format!(
                "(db.status IN ({pending}) AND (db.demanded OR db.status IN ({in_flight})))",
                pending = crate::status_sql::build_in(&BUILDER_STATUSES),
                in_flight = crate::status_sql::build_in(&[Queued, Building]),
            ),
            "the predicate is the Rust rule written out; changing one without the \
             other settles an evaluation whose builds are still running",
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

    /// Dependents direction must walk upward (a dependency edge leads to the
    /// anchors that consume it) so failure cascades reach every consumer.
    #[test]
    fn dependents_walk_upward() {
        let cte = norm(&dependency_closure_cte(
            "dependents",
            "SELECT $1::uuid",
            ClosureDirection::Dependents,
        ));
        assert!(
            cte.starts_with("WITH RECURSIVE dependents(derivation) AS"),
            "{cte}"
        );
        assert!(
            cte.contains(
                "SELECT e.derivation AS next FROM derivation_dependency e WHERE e.dependency = c.derivation"
            ),
            "must walk dependents upward via the dependency edge: {cte}"
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

    /// The gate needs the `.drv` PRESENT, not whole. A `.drv` closure is trusted:
    /// the evaluation pushes it before it reports the derivation, and the one
    /// `.drv` state a fresh evaluation repairs is an absent NAR, which is why the
    /// gate and [`drv_nar_absent_predicate`] are exact negations of each other.
    #[test]
    fn the_gate_needs_the_drv_present_not_whole() {
        let g = norm(&gates_predicate("db"));
        assert!(
            g.contains("cp.file_hash IS NOT NULL") && !g.contains("missing_references"),
            "{g}"
        );
        assert_eq!(
            norm(&drv_present_predicate("db")),
            norm(&drv_nar_absent_predicate("db")).trim_start_matches("NOT "),
            "the two must stay exact negations"
        );
    }

    /// Wholeness moved from the path to the anchor: present, with no runtime edge
    /// into something that is not whole itself. `fetchable` is its projection onto
    /// a terminal-success status and reads no path counter at all.
    #[test]
    fn whole_is_present_with_no_missing_runtime_dep_and_fetchable_reads_it() {
        assert_eq!(
            norm(&anchor_whole_predicate("db")),
            "(db.missing_runtime_deps = 0 AND (EXISTS (SELECT 1 FROM derivation_output o2 \
             WHERE o2.derivation = db.derivation) AND NOT EXISTS (SELECT 1 FROM derivation_output o \
             LEFT JOIN cached_path cp ON cp.hash = o.hash WHERE o.derivation = db.derivation \
             AND cp.file_hash IS NULL)))"
        );
        let f = norm(&fetchable_predicate("db"));
        assert!(f.contains("db.missing_runtime_deps = 0"), "{f}");
        assert!(
            !f.contains("missing_references"),
            "the path counter is no longer read: {f}"
        );
    }

    /// Fetchable is the one readiness fact dependents read, and since #593 it is
    /// about OUR cache only: terminal success with every output whole. An anchor
    /// an upstream happens to serve is not fetchable until its relay lands.
    #[test]
    fn fetchable_is_terminal_success_with_whole_outputs_in_our_own_cache() {
        let p = norm(&fetchable_predicate("db"));
        assert!(p.starts_with("(db.status IN (3, 7)"), "{p}");
        assert!(p.contains("cp.file_hash IS NULL"), "{p}");
        assert!(
            !p.contains("substitutable"),
            "an upstream copy is not our cache: {p}"
        );
        assert!(
            !p.contains("derivation_dependency"),
            "no walk over the build graph: {p}"
        );
    }

    /// The `NOT EXISTS` over the outputs is vacuously true for an anchor with no
    /// output rows, so terminal success alone would backfill `fetchable` on one
    /// and stop it counting toward its dependents' `unready_deps` - the
    /// unbacked-output dead zone. The `EXISTS` guard is the fix and must stay.
    #[test]
    fn an_anchor_with_no_outputs_is_not_fetchable() {
        let p = norm(&fetchable_predicate("db"));
        assert!(
            p.contains(
                "EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = db.derivation) AND NOT EXISTS"
            ),
            "the output guard must precede the NOT EXISTS: {p}"
        );
    }

    /// Both arms are demand-gated now. A build used to be promoted the moment its
    /// own inputs were fetchable, with nothing asking whether anything still wanted
    /// its output, which is why a relayed anchor's whole input closure was built
    /// (#666). The term is a column read, so the one-hop EXISTS that used to leave
    /// every promote and un-promote is gone, and #591's gate work with it.
    #[test]
    fn both_gate_arms_are_demand_gated_and_read_the_column() {
        let sql = norm(&gates_predicate("db"));
        assert!(sql.contains("db.demanded"), "{sql}");
        assert!(
            !sql.contains("FROM derivation_dependency"),
            "the gate must not walk edges per row: {sql}"
        );
        assert!(
            sql.contains("db.unready_deps = 0"),
            "the build arm keeps its readiness terms: {sql}"
        );
        assert!(
            sql.contains("FROM build_job bj WHERE bj.derivation = db.derivation"),
            "an unnamed anchor is still not promotable: {sql}"
        );
    }

    /// The demand arm reads a DEPENDENT's status, which is only safe while both
    /// promotion moves stay inside the set it tests: a demote-then-promote pair in
    /// one transaction would otherwise change what the second statement's gate
    /// sees for an unrelated row.
    #[test]
    fn the_demanded_status_set_is_closed_under_both_promotion_moves() {
        for status in [BuildStatus::Created, BuildStatus::Queued] {
            assert!(
                BUILDER_STATUSES.contains(&status),
                "{status:?} is an endpoint of a promotion move"
            );
        }
    }

    /// The gate has two arms and demand sits outside both: a relay needs nothing
    /// more, a build needs ready inputs and an importable `.drv`.
    #[test]
    fn gates_split_on_substitutable() {
        let g = norm(&gates_predicate("db"));
        assert!(
            g.contains(
                "AND db.demanded AND (db.substitutable OR (db.unready_deps = 0 AND EXISTS ("
            ),
            "demand is common to both arms, the split is on substitutable alone: {g}"
        );
        assert!(
            g.contains("JOIN cached_path cp ON cp.hash = d.hash WHERE d.id = db.derivation"),
            "the build arm reads the anchor's own .drv: {g}"
        );
    }

    /// Promotable is a per-row check: walked, wanted, and then either demand (a
    /// relay) or zero unready deps plus a whole `.drv` (a build). Dispatchable is
    /// the same on Queued.
    #[test]
    fn promotable_is_created_plus_the_gates() {
        let p = norm(&promotable_predicate("db"));
        assert!(p.starts_with("(db.status = 0 AND ("), "{p}");
        assert!(p.contains("w.walked"), "{p}");
        assert!(p.contains("db.unready_deps = 0"), "{p}");
        assert!(
            p.contains("FROM build_job bj WHERE bj.derivation = db.derivation"),
            "{p}"
        );
        assert!(
            !p.contains("derivation_input_source"),
            "sources are references of the .drv: {p}"
        );
    }

    /// The gates must not read the anchor's OWN `status`, or a demote-then-promote
    /// pair in one transaction (the readiness migrations, `readiness::repair_pending`)
    /// starts double-moving rows: the demote writes the column the promote would read.
    #[test]
    fn the_gates_never_read_the_anchors_own_status_column() {
        let gates = gates_predicate("db");
        assert!(!gates.contains("db.status"), "{gates}");
        assert!(
            promotable_predicate("db").contains("db.status = 0 AND"),
            "the status term belongs to promotable alone"
        );
    }

    /// Every promotion and dispatch gate reads the derivation's `walked` bit
    /// through this one predicate.
    #[test]
    fn walked_predicate_reads_the_derivation_row() {
        let p = norm(&walked_predicate("db"));
        assert_eq!(
            p,
            "EXISTS (SELECT 1 FROM derivation w WHERE w.id = db.derivation AND w.walked)"
        );
    }

    /// The orphan-GC keep-set must be the build-dependency closure of the live
    /// roots (entry_points + build_jobs), not just the roots themselves - a dep
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
                "dependents",
                "SELECT $1::uuid",
                ClosureDirection::Dependents,
            )),
            norm(&runtime_closure_cte("refs", "SELECT $1::uuid")),
            norm(&pending_closure_cte(
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
            "dependents",
            "SELECT $1::uuid",
            ClosureDirection::Dependents,
            "e.derivation IN (SELECT derivation FROM closure)",
        ));

        let probe = "WHERE e.dependency = c.derivation \
                     AND e.derivation IN (SELECT derivation FROM closure) OFFSET 0) s";

        assert!(
            cte.contains(probe),
            "the bound belongs inside the fenced probe: {cte}"
        );
    }

    /// The runtime closure walks what a client must fetch alongside an output,
    /// not the build inputs, so it is the same relation restricted to the runtime
    /// edge kinds.
    #[test]
    fn the_runtime_closure_walks_runtime_edges_only() {
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
    fn the_live_set_walks_runtime_edges_from_live_derivations_and_keeps_their_sources() {
        let cte = norm(&live_cached_paths_cte());
        assert!(cte.contains("e.kind IN (1, 2)"), "{cte}");
        assert!(
            cte.contains("SELECT s.hash FROM derivation_input_source s JOIN"),
            "{cte}"
        );
        assert!(!cte.contains("cached_path_reference"), "{cte}");
    }

    /// Demand and adoption read one definition of a builder, so an anchor an
    /// evaluation adopts is one whose gate demands its inputs, never the other
    /// way round.
    #[test]
    fn demand_and_adoption_share_one_definition_of_a_builder() {
        let builder = norm(&builder_predicate("p", "w"));
        assert_eq!(
            builder,
            "w.walked AND NOT p.substitutable AND p.status IN (0, 1, 2, 8)"
        );
        assert!(
            norm(&pending_closure_cte(
                "pending",
                "SELECT $1::uuid, $2::uuid, true"
            ))
            .contains(&norm(&builder_predicate("dep", "w")))
        );
    }

    /// The adoption walk carries the evaluation it walks for, steps only out of a
    /// builder, and only into anchors an evaluation will still have built: a relay
    /// or a terminal anchor is reached and never expanded.
    #[test]
    fn the_pending_closure_walks_dependencies_through_builders_only() {
        let cte = norm(&pending_closure_cte(
            "pending",
            "SELECT $1::uuid, $2::uuid, true",
        ));
        assert!(
            cte.starts_with(
                "WITH RECURSIVE pending(evaluation, derivation, builder) AS \
                 (SELECT $1::uuid, $2::uuid, true UNION \
                 SELECT c.evaluation, s.next, s.builder FROM pending c, LATERAL ("
            ),
            "{cte}"
        );
        assert!(
            cte.contains(
                "SELECT e.dependency AS next, \
                 (w.walked AND NOT dep.substitutable AND dep.status IN (0, 1, 2, 8)) AS builder"
            ),
            "{cte}"
        );
        assert!(
            cte.contains(
                "WHERE e.derivation = c.derivation AND c.builder AND e.kind IN (0, 2) \
                 AND dep.status IN (0, 1, 2, 8) OFFSET 0) s"
            ),
            "{cte}"
        );
    }

    /// Demand flows DOWN from the entry points through named builders, which is
    /// the arm the one-hop predicate it replaced could not carry: `Created` counts
    /// as a builder, so without the recursion the first undemanded builder
    /// re-demands everything below it (#666). A relay is reached and never stepped
    /// through, and that single fact is what stops a relayed subtree being built.
    #[test]
    fn the_build_arm_steps_only_out_of_named_builders() {
        let sql = norm(&demand_closure_cte(
            "SELECT derivation FROM entry_point",
            "",
        ));
        assert!(
            sql.starts_with(
                "WITH RECURSIVE demanded(derivation) AS (SELECT derivation FROM entry_point UNION"
            ),
            "{sql}"
        );
        assert!(sql.contains(&norm(&builder_predicate("p", "w"))), "{sql}");
        assert!(
            sql.contains("SELECT e.dependency AS next FROM derivation_dependency e"),
            "the step must project the dependency, not the dependent: {sql}"
        );
        assert!(
            sql.contains("FROM build_job bj WHERE bj.derivation = p.derivation"),
            "an unnamed builder demands nothing: {sql}"
        );
        assert!(
            sql.contains("OFFSET 0"),
            "the lateral fence must survive: {sql}"
        );
    }

    /// Two arms, one definition: a demanded anchor named by a build_job demands the
    /// producers over its runtime edges; only a demanded builder demands over its
    /// build edges. A relay's build inputs are never reached.
    #[test]
    fn demand_steps_over_runtime_edges_from_any_anchor_and_build_edges_from_builders() {
        let cte = norm(&demand_closure_cte(
            "SELECT derivation FROM entry_point",
            "",
        ));
        assert!(
            cte.contains("WHERE e.derivation = c.derivation AND e.kind IN (1, 2)"),
            "{cte}"
        );
        assert!(
            cte.contains(
                "WHERE e.derivation = c.derivation AND e.kind IN (0, 2) AND w.walked AND NOT p.substitutable"
            ),
            "{cte}"
        );
    }

    #[test]
    fn a_region_bounds_both_arms_of_the_demand_walk() {
        let cte = norm(&demand_closure_cte(
            "SELECT derivation FROM roots",
            "SELECT derivation FROM region",
        ));
        assert_eq!(
            cte.matches("AND e.dependency IN (SELECT derivation FROM region)")
                .count(),
            2,
            "{cte}"
        );
    }

    /// A region is every anchor whose demand an event can have moved, so it steps
    /// over the runtime edges of every member and over the build edges of the ones
    /// that are builders.
    #[test]
    fn the_region_steps_over_runtime_edges_out_of_every_pending_member() {
        let cte = norm(&pending_closure_cte(
            "region",
            "SELECT NULL::uuid, unnest($1::uuid[]), true",
        ));
        assert!(
            cte.contains("e.kind IN (1, 2)") && cte.contains("c.builder AND e.kind IN (0, 2)"),
            "{cte}"
        );
    }
}
