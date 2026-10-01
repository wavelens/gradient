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

use gradient_entity::build::BuildStatus;
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
/// `.drv`. Negated from [`drv_present_predicate`] rather than restated, so the
/// two cannot drift: a `.drv` that is present but whose closure has a missing dependency is
/// deliberately NOT this, because re-evaluating cannot fetch that dependency, and
/// conflating the two burned an evaluation per stall and then failed it as
/// unrecoverable with the `.drv` cached the whole time.
pub fn drv_nar_absent_predicate(alias: &str) -> String {
    format!("NOT {}", drv_present_predicate(alias))
}

/// The shared build `{alias}`'s derivation has its full record in. Promotion and
/// dispatch require it: a shared build whose edges are not all recorded would
/// otherwise be queued as dependency-free.
pub fn walked_predicate(alias: &str) -> String {
    format!("EXISTS (SELECT 1 FROM derivation w WHERE w.id = {alias}.derivation AND w.walked)")
}

/// The statuses of a shared build that will still be built, and so still needs its
/// inputs. Closed under both promotion moves (`Created` to `Queued` and back), so a
/// promotion can never change whether a parent needs what it walks over.
pub const BUILDER_STATUSES: [BuildStatus; 4] = [
    BuildStatus::Created,
    BuildStatus::Queued,
    BuildStatus::Building,
    BuildStatus::FailedTransient,
];

/// The statuses at which the need flag decides whether an evaluation still waits: every
/// builder status plus `Skipped` and `Aborted`, which are thawed BY the need
/// returning. What a walk reaches is not this set but [`open_predicate`], which a
/// terminal-success shared build satisfies too while it cannot be fetched.
pub const NEED_BUILD_STATUSES: [BuildStatus; 6] = [
    BuildStatus::Created,
    BuildStatus::Queued,
    BuildStatus::Building,
    BuildStatus::FailedTransient,
    BuildStatus::Skipped,
    BuildStatus::Aborted,
];

/// Shared build `status`/`wanted` is work its evaluation is still waiting for: what
/// nothing needs is never promoted, so an evaluation that counts it as pending
/// waits forever (#666). Every terminal status is settled by definition, and a
/// shared build the dispatcher can still pick up - or already has - is work in flight
/// whatever the need flag says now: dispatch reads the status rather than the gate, and
/// a running build is deliberately left to finish.
///
/// The set is [`NEED_BUILD_STATUSES`], so a wanted `Skipped` shared build blocks and an
/// unwanted one does not, which is the whole meaning of the status. The need is
/// written before the thaw that follows it, and an evaluation settled in that gap
/// is settled over work it still owes.
pub fn blocks_evaluation(status: BuildStatus, wanted: bool) -> bool {
    if !NEED_BUILD_STATUSES.contains(&status) {
        return false;
    }

    wanted || matches!(status, BuildStatus::Queued | BuildStatus::Building)
}

/// [`blocks_evaluation`] as a predicate on shared build `{alias}`, so the decision has
/// one definition and the reader that asks the database for it cannot drift from
/// the reader that evaluates it in Rust.
pub fn blocks_evaluation_predicate(alias: &str) -> String {
    format!(
        "({alias}.status IN ({pending}) AND ({alias}.wanted OR {alias}.status IN ({in_flight})))",
        pending = crate::status_sql::build_in(&NEED_BUILD_STATUSES),
        in_flight = crate::status_sql::build_in(&[BuildStatus::Queued, BuildStatus::Building]),
    )
}

/// Shared build `{shared_build}` (its `derivation` row aliased `{walked}`) is a builder:
/// recorded, answered by the upstream probe, not a passthrough, and in a status an
/// evaluation will still have built. The one definition of what needs its
/// inputs and of what the adoption walk steps through, so the two can never
/// disagree.
///
/// `probed` is what makes "not a passthrough" a fact rather than a guess. The probe is
/// network and stays off every graph path, so a shared build is unprobed for as long as
/// a round takes; reading that as "will be built" needed the build closure of
/// every output an upstream serves, and the dispatcher hands those out inside the
/// window. The passthrough that follows withdraws the need, but a job already handed
/// to a worker keeps running to its end, and an input that cannot be fetched fails the
/// evaluation that no longer needed it.
pub fn builder_predicate(shared_build: &str, walked: &str) -> String {
    format!(
        "{walked}.walked AND {shared_build}.probed AND NOT {shared_build}.cache_available \
         AND {shared_build}.status IN ({pending})",
        pending = crate::status_sql::build_in(&BUILDER_STATUSES),
    )
}

/// Shared build `{alias}` is open: a parent cannot fetch it from our cache and no
/// verdict stands against it. Every walk reaches open shared builds and stops at the
/// rest, because what is below a fetchable shared build is served from our cache and
/// what is below a terminal failure is the requeue's to thaw. A terminal-success
/// shared build whose outputs are gone or whose closure has a missing dependency is open, and that is
/// the only way the missing dependency below it is reached at all; so is an aborted one, since
/// an abort is not a verdict and a live want is what thaws it.
pub fn open_predicate(alias: &str) -> String {
    format!(
        "(NOT {alias}.fetchable AND {alias}.status NOT IN ({failed}))",
        failed = crate::status_sql::build_in(&BuildStatus::TERMINAL_FAILURE),
    )
}

/// The shared build on derivation `{derivation}` still needs its inputs: it is no
/// passthrough. A failure walk stops at one that is, because a shared build available in a cache takes
/// finished bytes off an upstream cache: an input that can never build neither dooms it
/// nor reaches anything above it. Same rule as [`gates_predicate`]'s passthrough arm,
/// which lets a passthrough through with its `blocking_deps` unread, and as the refusal in
/// `graph::policy` to mark a failing passthrough `Permanent`.
pub fn non_passthrough_predicate(derivation: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM derivation_build rb \
         WHERE rb.derivation = {derivation} AND rb.cache_available)"
    )
}

/// Parents can get the outputs of shared build `{alias}` from OUR cache: it reached
/// terminal success and every output has a complete closure here. An upstream copy does not
/// count (#593): a parent of a shared build that is available in a cache and not yet passed through waits for the
/// passthrough, so a build pulls every input out of our own cache and the passthrough is on
/// the critical path exactly once, instead of every parent re-fetching from
/// the upstream cache itself. This is the one can-start fact a parent reads, and it
/// recurses over nothing: a dependency's own dependencies are already summarised
/// in its `blocking_deps`.
///
/// The `EXISTS` over `derivation_output` is load-bearing and not a tautology. The
/// `NOT EXISTS` under it is vacuously true for a shared build with NO output rows, so
/// without the guard a terminal-success shared build whose outputs were never recorded
/// reads as fetchable, stops counting toward its parents' `blocking_deps`, and
/// those parents are promoted and dispatched against an input nothing can
/// provide: the unbacked-output dead zone, measured on a live cluster.
pub fn fetchable_predicate(alias: &str) -> String {
    format!(
        "({alias}.status IN ({terminal_success}) AND {complete})",
        terminal_success = crate::status_sql::build_in(&BuildStatus::TERMINAL_SUCCESS),
        complete = shared_build_complete_predicate(alias),
    )
}

/// Every output of `{alias}` has its NAR in our cache.
///
/// The `EXISTS` is load-bearing and not a tautology. The `NOT EXISTS` under it is
/// vacuously true for a shared build with NO output rows, so without the guard a
/// terminal-success shared build whose outputs were never recorded reads as present,
/// stops counting toward its parents' `blocking_deps`, and those parents are
/// promoted and dispatched against an input nothing can provide: the
/// unbacked-output dead zone, measured on a live cluster.
pub fn present_predicate(alias: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = {alias}.derivation) \
         AND NOT EXISTS (SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash \
                         WHERE o.derivation = {alias}.derivation AND cp.file_hash IS NULL))"
    )
}

/// [`present_predicate`] as one probe of the outputs, for a projection: a
/// `RETURNING` pays both of the predicate's subplans per row, where a `WHERE`
/// would have semi-joined them. No output rows aggregate to NULL, which is absent.
pub fn present_value(alias: &str) -> String {
    format!(
        "coalesce((SELECT bool_and(cp.file_hash IS NOT NULL) FROM derivation_output o \
                   LEFT JOIN cached_path cp ON cp.hash = o.hash \
                   WHERE o.derivation = {alias}.derivation), false)"
    )
}

/// Present, and every runtime dependency leads to a complete shared build: the whole runtime
/// closure of `{alias}`'s outputs is in our cache. The counter is moved by
/// [`crate::runtime_can_start`], never derived here, for the reason `blocking_deps`
/// is: a complete closure is transitive and a per-row predicate that looks one hop cannot
/// carry it.
pub fn shared_build_complete_predicate(alias: &str) -> String {
    format!(
        "({alias}.missing_runtime_deps = 0 AND {present})",
        present = present_predicate(alias),
    )
}

/// The gates a `Created` shared build must pass to be queued, minus its own status term:
/// walked, named by some evaluation, still needed, then one arm per kind of work.
/// A passthrough needs nothing more: it fetches finished bytes, so neither its inputs nor
/// its `.drv` matter. A build needs every input fetchable from our cache and its own
/// `.drv` importable.
///
/// The need is a column, not a walk: [`crate::can_start::update_need`] rewrites it
/// on the events that change it and the consistency check rewrites it absolutely.
/// Reading it here is what makes it transitive, which the one-hop `EXISTS` this
/// replaced could not be: a `Created` parent counts as a builder, so the first
/// unwanted one re-wanted everything below it and a passed-through shared build's whole input
/// closure was built (#666). It also takes a correlated three-table subquery off
/// every promote, un-promote and sweep row.
///
/// `probed` is on the build arm for the reason it is on [`builder_predicate`], one level
/// up: a shared build reached over a RUNTIME dependency is needed whether or not anything will
/// build it, so it never passes through that predicate and arrives here unanswered. A
/// source FOD under a passed-through output is exactly that shape, and the dispatch loop
/// beats the probe tick: busybox's tarball was queued 60 ms after the round that
/// would have asked for it, built, and failed offline on an upstream cache that was
/// serving it. The passthrough arm needs no such term, since nothing is available in a cache
/// until the probe says so.
///
/// It must stay free of any reference to `{alias}`'s OWN `status`, which is why that
/// term lives in [`promotable_predicate`] instead. `m20260908_000002` and
/// `m20260909_000001` execute a demote and a promote in sequence in one transaction and
/// they cannot interfere only because this never reads the column the demote writes;
/// the same holds for [`crate::can_start::repair_can_start`].
pub fn gates_predicate(alias: &str) -> String {
    format!(
        r#"({walked}
    AND EXISTS (SELECT 1 FROM build_job bj WHERE bj.derivation = {alias}.derivation)
    AND {alias}.wanted
    AND ({alias}.cache_available
         OR ({alias}.probed AND {alias}.blocking_deps = 0 AND {drv_present})))"#,
        walked = walked_predicate(alias),
        drv_present = drv_present_predicate(alias),
    )
}

/// [`gates_predicate`] on a `Created` shared build: what promotion writes.
///
/// Dispatch does not re-derive this: it reads the status. So the invariant rests on
/// one rule, which every writer of `Queued` obeys in one of two ways: embed this
/// predicate in the write, or settle the rows just written with
/// [`crate::can_start::unpromote_ungated`] in the same call.
/// [`crate::can_start::repair_can_start`] is the backstop for a counter that drifted
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

    /// A shared build nothing needs is never promoted, so an evaluation that waits
    /// for it never finishes: it is settled work, not pending work. `Queued` and
    /// `Building` are the exception in the other direction: the dispatcher reads
    /// the status, so both can still produce a build after the need is gone.
    #[test]
    fn only_wanted_or_in_flight_work_blocks_an_evaluation() {
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
                "{status:?} is handed out on its status alone, wanted or not"
            );
        }
        for status in [Created, FailedTransient, Skipped, Aborted] {
            assert!(
                !blocks_evaluation(status, false),
                "{status:?} with nothing wanting it is work that never happens"
            );
        }
        assert!(
            blocks_evaluation(Skipped, true),
            "a skipped shared build that is wanted again is owed work: the need is \
             written before the thaw, and an evaluation settled in that gap is \
             settled over a subtree it is about to queue"
        );
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
                "(db.status IN ({pending}) AND (db.wanted OR db.status IN ({in_flight})))",
                pending = crate::status_sql::build_in(&NEED_BUILD_STATUSES),
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

    /// The gate needs the `.drv` PRESENT, not complete. A `.drv` closure is trusted:
    /// the evaluation pushes it before it reports the derivation, and the one
    /// `.drv` state a fresh evaluation repairs is an absent NAR, which is why the
    /// gate and [`drv_nar_absent_predicate`] are exact negations of each other.
    #[test]
    fn the_gate_needs_the_drv_present_not_complete() {
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

    /// The complete closure moved from the path to the shared build: present, with no runtime dependency
    /// on something that is not complete itself. `fetchable` is its projection onto
    /// a terminal-success status and reads no path counter at all.
    #[test]
    fn complete_is_present_with_no_missing_runtime_dep_and_fetchable_reads_it() {
        assert_eq!(
            norm(&shared_build_complete_predicate("db")),
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

    /// Fetchable is the one can-start fact parents read, and since #593 it is
    /// about OUR cache only: terminal success with every output complete. A shared build
    /// an upstream cache happens to serve is not fetchable until its passthrough lands.
    #[test]
    fn fetchable_is_terminal_success_with_complete_outputs_in_our_own_cache() {
        let p = norm(&fetchable_predicate("db"));
        assert!(p.starts_with("(db.status IN (3, 7)"), "{p}");
        assert!(p.contains("cp.file_hash IS NULL"), "{p}");
        assert!(
            !p.contains("cache_available"),
            "an upstream copy is not our cache: {p}"
        );
        assert!(
            !p.contains("derivation_dependency"),
            "no walk over the build graph: {p}"
        );
    }

    /// The `NOT EXISTS` over the outputs is vacuously true for a shared build with no
    /// output rows, so terminal success alone would backfill `fetchable` on one
    /// and stop it counting toward its parents' `blocking_deps`: the
    /// unbacked-output dead zone. The `EXISTS` guard is the fix and must stay.
    #[test]
    fn a_shared_build_with_no_outputs_is_not_fetchable() {
        let p = norm(&fetchable_predicate("db"));
        assert!(
            p.contains(
                "EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = db.derivation) AND NOT EXISTS"
            ),
            "the output guard must precede the NOT EXISTS: {p}"
        );
    }

    /// Both arms are gated by the need flag now. A build used to be promoted the moment its
    /// own inputs were fetchable, with nothing asking whether anything still wanted
    /// its output, which is why a passed-through shared build's whole input closure was built
    /// (#666). The term is a column read, so the one-hop EXISTS that used to leave
    /// every promote and un-promote is gone, and #591's gate work with it.
    #[test]
    fn both_gate_arms_are_need_gated_and_read_the_column() {
        let sql = norm(&gates_predicate("db"));
        assert!(sql.contains("db.wanted"), "{sql}");
        assert!(
            !sql.contains("FROM derivation_dependency"),
            "the gate must not walk edges per row: {sql}"
        );
        assert!(
            sql.contains("db.blocking_deps = 0"),
            "the build arm keeps its can-start terms: {sql}"
        );
        assert!(
            sql.contains("FROM build_job bj WHERE bj.derivation = db.derivation"),
            "an unnamed shared build is still not promotable: {sql}"
        );
    }

    /// `Skipped` is settled work: no gate acts on it, no walk steps THROUGH it and
    /// no evaluation waits for it. It is in none of the sets that decide any of
    /// those, and the thaw back to `Created` is what re-opens them at once. Being
    /// REACHED is the one thing it must still be, because the need returning is its
    /// thaw and a walk that cannot reach the row cannot write the column.
    #[test]
    fn skipped_is_neither_a_builder_nor_pending_nor_terminal() {
        assert!(NEED_BUILD_STATUSES.contains(&BuildStatus::Skipped));
        assert!(!BUILDER_STATUSES.contains(&BuildStatus::Skipped));
        assert!(!BuildStatus::PENDING.contains(&BuildStatus::Skipped));
        assert!(!BuildStatus::TERMINAL_SUCCESS.contains(&BuildStatus::Skipped));
        assert!(!BuildStatus::FAILURE.contains(&BuildStatus::Skipped));
        assert!(!BuildStatus::REQUEUEABLE.contains(&BuildStatus::Skipped));
    }

    /// The need arm reads a PARENT's status, which is only safe while both
    /// promotion moves stay inside the set it tests: a demote-then-promote pair in
    /// one transaction would otherwise change what the second statement's gate
    /// sees for an unrelated row.
    #[test]
    fn the_wanted_status_set_is_closed_under_both_promotion_moves() {
        for status in [BuildStatus::Created, BuildStatus::Queued] {
            assert!(
                BUILDER_STATUSES.contains(&status),
                "{status:?} is an endpoint of a promotion move"
            );
        }
    }

    /// The gate has two arms and the need sits outside both: a passthrough needs nothing
    /// more, a build needs fetchable inputs and an importable `.drv`.
    #[test]
    fn gates_split_on_cache_available() {
        let g = norm(&gates_predicate("db"));
        assert!(
            g.contains(
                "AND db.wanted AND (db.cache_available OR (db.probed AND db.blocking_deps = 0 AND EXISTS ("
            ),
            "need is common to both arms, the split is on cache_available alone: {g}"
        );
        assert!(
            g.contains("JOIN cached_path cp ON cp.hash = d.hash WHERE d.id = db.derivation"),
            "the build arm reads the shared build's own .drv: {g}"
        );
    }

    /// Promotable is a per-row check: walked, wanted, and then either the need (a
    /// passthrough) or zero blocking deps plus a present `.drv` (a build). Dispatchable is
    /// the same on Queued.
    #[test]
    fn promotable_is_created_plus_the_gates() {
        let p = norm(&promotable_predicate("db"));
        assert!(p.starts_with("(db.status = 0 AND ("), "{p}");
        assert!(p.contains("w.walked"), "{p}");
        assert!(p.contains("db.blocking_deps = 0"), "{p}");
        assert!(
            p.contains("FROM build_job bj WHERE bj.derivation = db.derivation"),
            "{p}"
        );
        assert!(
            !p.contains("derivation_input_source"),
            "sources are references of the .drv: {p}"
        );
    }

    /// The gates must not read the shared build's OWN `status`, or a demote-then-promote
    /// pair in one transaction (the can-start migrations, `can_start::repair_can_start`)
    /// starts double-moving rows: the demote writes the column the promote would read.
    #[test]
    fn the_gates_never_read_the_shared_builds_own_status_column() {
        let gates = gates_predicate("db");
        assert!(!gates.contains("db.status"), "{gates}");
        assert!(
            promotable_predicate("db").contains("db.status = 0 AND"),
            "the status term belongs to promotable alone"
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

    /// Open is the one reach condition of every walk: not fetchable, and no verdict
    /// against it. It reads two columns and no subquery, so a walk pays one row
    /// lookup per reached shared build; a terminal-success shared build whose closure has a
    /// missing dependency satisfies it like a builder does, and so does an aborted one.
    #[test]
    fn open_is_not_fetchable_and_not_a_terminal_failure() {
        assert_eq!(
            norm(&open_predicate("db")),
            format!(
                "(NOT db.fetchable AND db.status NOT IN ({}))",
                crate::status_sql::build_in(&BuildStatus::TERMINAL_FAILURE)
            )
        );
        for status in NEED_BUILD_STATUSES {
            assert!(
                !BuildStatus::TERMINAL_FAILURE.contains(&status),
                "{status:?} is open while it is not fetchable"
            );
        }
        for status in BuildStatus::TERMINAL_SUCCESS {
            assert!(
                !BuildStatus::TERMINAL_FAILURE.contains(&status),
                "{status:?} is open while it is not fetchable: its closure misses a dependency"
            );
        }
    }

    /// An abort is not a verdict. It stays in `REQUEUEABLE`, because an evaluation
    /// whose shared build sat aborted did not get it built, and it can be needed, because
    /// the need returning is what thaws it: twelve aborted shared builds inside a runtime
    /// closure were the wall behind 47 builders, and no requeue ever revisited them.
    #[test]
    fn an_aborted_shared_build_is_open_and_thawed_by_need() {
        assert!(NEED_BUILD_STATUSES.contains(&BuildStatus::Aborted));
        assert!(BuildStatus::REQUEUEABLE.contains(&BuildStatus::Aborted));
        assert!(!BuildStatus::TERMINAL_FAILURE.contains(&BuildStatus::Aborted));
        assert!(blocks_evaluation(BuildStatus::Aborted, true));
        assert!(!blocks_evaluation(BuildStatus::Aborted, false));
    }

    /// The walk carries the evaluation it walks for and the builder bit of the row
    /// it stands on, reaches open shared builds only, and steps out of a member over its
    /// runtime dependencies and out of a builder over every edge. One probe per member,
    /// so the two edge kinds are one index range scan and not two.
    #[test]
    fn the_walk_reaches_open_shared_builds_and_steps_every_edge_out_of_a_builder() {
        let cte = norm(&open_closure_cte(
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
            cte.contains(&format!(
                "SELECT e.dependency AS next, ({}) AS builder",
                norm(&builder_predicate("dep", "w"))
            )),
            "{cte}"
        );
        assert!(
            cte.contains(&format!(
                "WHERE e.derivation = c.derivation AND (c.builder OR e.kind IN (1, 2)) \
                 AND {} OFFSET 0) s",
                norm(&open_predicate("dep"))
            )),
            "{cte}"
        );
        assert_eq!(cte.matches("LATERAL (").count(), 1, "one probe: {cte}");
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
